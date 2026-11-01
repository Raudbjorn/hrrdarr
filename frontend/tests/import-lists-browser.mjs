// Owned library_ui_fixture only: real HTTP and real clicks against the real import-list router and
// scheduler. The fixture serves JSON feeds at ${list_origin}/feed/{tv,movies,broken}; every claim is
// read back through the production library and import-list APIs.
import assert from 'node:assert/strict';
import {mkdir} from 'node:fs/promises';
import {isAbsolute,join} from 'node:path';
import {pathToFileURL} from 'node:url';
const {chromium}=await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE||'/usr/lib/node_modules/playwright/index.mjs').href);
const origin=process.env.UI_URL,fixture=JSON.parse(process.env.UI_FIXTURE_JSON||'{}');
assert.equal(new URL(origin).hostname,'127.0.0.1');assert.equal(new URL(fixture.api).hostname,'127.0.0.1');
assert.ok(fixture.list_origin&&fixture.tv_path&&fixture.movie_path,'fixture must provide list_origin, tv_path and movie_path');
const artifacts=process.env.UI_SCREENSHOT_DIR;
assert.ok(artifacts&&isAbsolute(artifacts)&&/\.do-not-commit\/loop\/iteration122(?:[-/]|$)/.test(artifacts),'UI_SCREENSHOT_DIR must be an owned iteration122 artifact directory');
await mkdir(artifacts,{recursive:true});
const TOKEN='bearer-secret-token-9f3a';
const browser=await chromium.launch({headless:true,...(process.env.PLAYWRIGHT_EXECUTABLE?{executablePath:process.env.PLAYWRIGHT_EXECUTABLE}:{})});
const page=await (await browser.newContext({viewport:{width:1280,height:1000}})).newPage();page.setDefaultTimeout(20000);
const errors=[];page.on('pageerror',e=>errors.push(e.message));
page.on('dialog',d=>{void d.accept();});
const panel=page.getByRole('region',{name:'Import lists',exact:true});
const exclusions=page.getByRole('region',{name:'Import list exclusions',exact:true});
async function api(method,path,data,status=200){
 const r=await page.request[method](origin+path,{...(data===undefined?{}:{data}),timeout:10000});
 const ok=Array.isArray(status)?status.includes(r.status()):r.status()===status;
 assert.ok(ok,`${method} ${path}: ${r.status()} ${await r.text()}`);
 return r.status()===204?null:r.json();
}
async function until(read,accept,label,ms=30000){
 const deadline=Date.now()+ms;let last;
 while(Date.now()<deadline){last=await read();if(accept(last))return last;await page.waitForTimeout(200);}
 assert.fail(`${label}: ${JSON.stringify(last)}`);
}
const library=async domain=>{const all=[];for(let o=0;;o+=100){const p=await api('get',`/api/v1/${domain==='tv'?'tv/series':'movies'}?limit=100&offset=${o}`);all.push(...p.items);if(all.length>=p.total||p.items.length===0)return all;}};
const lists=async()=>(await api('get','/api/v1/importlist')).items;
const items=async id=>(await api('get',`/api/v1/importlist/${id}/items?limit=100`)).items;
const noOverflow=async label=>assert.deepEqual(await page.evaluate(()=>({sw:document.documentElement.scrollWidth,iw:window.innerWidth})).then(v=>v.sw<=v.iw),true,`horizontal overflow: ${label}`);
async function rootFolder(domain,path){
 const r=await page.request.post(`${origin}/api/v1/${domain}/root-folders`,{data:{path}});
 if(r.status()===201)return r.json();
 const all=await api('get',`/api/v1/${domain}/root-folders?limit=50`);
 const found=all.items.find(x=>x.path===path);assert.ok(found,`root folder ${path}: ${r.status()}`);return found;
}
async function addList(impl,media){
 await panel.getByLabel('Implementation',{exact:true}).selectOption({label:impl});
 await panel.getByLabel('List media type',{exact:true}).selectOption(media);
 await panel.getByRole('button',{name:'Add list',exact:true}).click();
}
async function defaults(form,{profile,root,tag}){
 await form.getByLabel('Quality profile',{exact:true}).selectOption({label:profile});
 await form.getByLabel('Root folder',{exact:true}).selectOption({label:root});
 if(tag)await form.getByRole('group',{name:'Tags',exact:true}).getByLabel(tag,{exact:true}).check();
}
const status=name=>panel.getByRole('status',{name:`Sync status ${name}`,exact:true});
try{
 // Real prerequisites through public APIs.
 const profile=async(domain,name)=>api('post',`/api/v1/${domain}/quality-profiles`,{name,items:[{kind:'quality',quality_id:4,allowed:true},{kind:'quality',quality_id:3,allowed:true}]},201);
 const tvProfile=await profile('tv','Lists TV profile'),movieProfile=await profile('movies','Lists movie profile');
 const tvRoot=await rootFolder('tv',fixture.tv_path),movieRoot=await rootFolder('movies',fixture.movie_path);
 const tvTag=await api('post','/api/v1/tv/tags',{label:'from-list'},[200,201]);
 await api('post','/api/v1/movies/tags',{label:'movie-list'},[200,201]);

 await page.goto(`${origin}/#/settings/lists`);
 await panel.waitFor();
 await panel.getByText('No import lists are configured.').waitFor();
 await panel.getByText('No exclusions.').waitFor();
 // Navigation entries: settings index and workspace nav.
 await page.goto(`${origin}/#/settings`);
 await page.getByRole('navigation',{name:'Settings sections'}).getByRole('link',{name:'Import lists',exact:true}).click();
 await panel.waitFor();
 await page.screenshot({path:join(artifacts,'import-lists-empty.png'),fullPage:true});

 // --- TV list: schema-driven form, write-only token, test from the draft, save.
 const implementations=await until(()=>panel.getByLabel('Implementation',{exact:true}).locator('option').allTextContents(),o=>o.length>0,'schema options');
 assert.ok(implementations.includes('Custom list'),`implementations: ${implementations}`);
 await addList('Custom list','tv');
 const form=panel.getByRole('form',{name:'Add Custom list'});
 await form.waitFor();
 assert.equal(await form.getByLabel('Series type',{exact:true}).count(),1);
 assert.equal(await form.getByLabel('Minimum availability',{exact:true}).count(),0,'movie-only field hidden for TV');
 assert.deepEqual(await form.getByLabel('Monitor',{exact:true}).locator('option').allTextContents(),['All','None','First season only','Last season only']);
 await form.getByLabel('Name',{exact:true}).fill('TV feed');
 await form.getByLabel(/^List URL/).fill(`${fixture.list_origin}/feed/tv`);
 const token=form.getByLabel('Bearer token',{exact:true});
 assert.equal(await token.getAttribute('type'),'password');
 await token.fill(TOKEN);
 await form.getByRole('button',{name:'Save',exact:true}).click();
 assert.match(await panel.getByRole('alert').first().innerText(),/Automatic add requires a quality profile and a root folder/,'client mirror blocks auto-add without defaults');
 await defaults(form,{profile:'Lists TV profile',root:fixture.tv_path,tag:'from-list'});
 await form.getByRole('button',{name:'Test',exact:true}).click();
 await panel.getByRole('status',{name:'Test result'}).waitFor();
 assert.match(await panel.getByRole('status',{name:'Test result'}).innerText(),/Reachable: 2 items found/);
 assert.deepEqual(await lists(),[],'a draft test must not save anything');
 await page.screenshot({path:join(artifacts,'import-lists-add-form.png'),fullPage:true});
 await form.getByRole('button',{name:'Save',exact:true}).click();
 await panel.getByText('Import list "TV feed" saved.').waitFor();
 const saved=(await lists())[0];
 assert.equal(saved.media_type,'tv');assert.deepEqual(saved.secrets_configured,['bearer_token']);assert.deepEqual(saved.tag_ids,[tvTag.id]);
 assert.equal(saved.quality_profile_id,tvProfile.id);assert.equal(saved.root_folder_id,tvRoot.id);
 assert.ok(!JSON.stringify(await lists()).includes(TOKEN),'the API never returns the token');
 assert.ok(!(await page.content()).includes(TOKEN),'the DOM never contains the token');

 // --- Sync now: queued/running -> succeeded with added=2 (observed through the DOM).
 await page.evaluate(()=>{window.__seen=[];new MutationObserver(()=>{for(const el of document.querySelectorAll('[role=status][aria-label^="Sync status"]')){const t=el.textContent.split(':')[0];if(window.__seen.at(-1)!==t)window.__seen.push(t);}}).observe(document.body,{subtree:true,childList:true,characterData:true});});
 await panel.getByRole('button',{name:'Sync now TV feed',exact:true}).click();
 await status('TV feed').getByText(/^(Queued|Running)/).waitFor({timeout:4000}).catch(async()=>assert.fail(`queued/running not shown: ${await page.evaluate(()=>window.__seen)}`));
 await status('TV feed').getByText(/^Succeeded: fetched 2, added 2, already in library 0, excluded 0, unresolved 0, failed 0, pending 0/).waitFor();
 const seen=await page.evaluate(()=>window.__seen);
 assert.ok(seen.some(s=>s==='Queued'||s==='Running')&&seen.at(-1)==='Succeeded',`status sequence ${seen}`);
 await page.screenshot({path:join(artifacts,'import-lists-synced.png'),fullPage:true});
 const tvSeries=await library('tv');
 const added=tvSeries.filter(s=>[202,203].includes(s.tvdb_id));
 assert.equal(added.length,2,`series: ${JSON.stringify(tvSeries.map(s=>s.tvdb_id))}`);
 for(const s of added){assert.equal(s.settings.quality_profile_id,tvProfile.id);assert.deepEqual(s.tag_ids,[tvTag.id]);assert.ok(s.path.startsWith(fixture.tv_path),s.path);}
 const tvList=(await lists())[0];
 assert.equal(tvList.last_error,null);assert.ok(tvList.last_success_at);
 assert.deepEqual((await items(tvList.id)).map(i=>i.state),['added','added']);
 // Items view and recent syncs.
 await panel.getByRole('button',{name:'Items TV feed',exact:true}).click();
 const itemsView=panel.getByRole('group',{name:'Items for TV feed',exact:true});
 await itemsView.getByText('Search series 202').waitFor();
 assert.equal(await itemsView.getByRole('list',{name:'List items'}).getByText('Added',{exact:true}).count(),2);
 await itemsView.getByLabel('Item state',{exact:true}).selectOption('failed');
 await itemsView.getByText(/No items in this state/).waitFor();
 await itemsView.getByLabel('Item state',{exact:true}).selectOption('added');
 await itemsView.getByText('Search series 203').waitFor();
 await panel.getByRole('button',{name:'Recent syncs TV feed',exact:true}).click();
 await panel.getByRole('group',{name:'Recent syncs for TV feed',exact:true}).getByText(/\(manual\): Succeeded: fetched 2, added 2/).waitFor();
 await page.screenshot({path:join(artifacts,'import-lists-items.png'),fullPage:true});
 await panel.getByRole('button',{name:'Items TV feed',exact:true}).click();
 await panel.getByRole('button',{name:'Recent syncs TV feed',exact:true}).click();

 // --- Second sync: finished items keep their state, so nothing is added again and no duplicates appear.
 await panel.getByRole('button',{name:'Sync now TV feed',exact:true}).click();
 await status('TV feed').getByText(/^Succeeded: fetched 2, added 0, already in library 0, excluded 0, unresolved 0, failed 0, pending 0/).waitFor();
 assert.equal((await library('tv')).filter(s=>[202,203].includes(s.tvdb_id)).length,2);
 assert.deepEqual((await items(tvList.id)).map(i=>i.state),['added','added'],'finished items keep their state');

 // --- Exclusion for a movie id; a movie list skips it and adds the other.
 await exclusions.getByLabel('Exclusion media type',{exact:true}).selectOption('movies');
 await exclusions.getByLabel('TMDb ID',{exact:true}).fill('202');
 await exclusions.getByLabel('Title',{exact:true}).fill('Search movie 202');
 await exclusions.getByLabel('Year (optional)',{exact:true}).fill('2021');
 await exclusions.getByRole('button',{name:'Add exclusion',exact:true}).click();
 await exclusions.getByText(/Excluded "Search movie 202" \(Movies 202\)/).waitFor();
 let ex=(await api('get','/api/v1/importlist/exclusions?media_type=movies')).items;
 assert.deepEqual(ex.map(e=>[e.external_id,e.title,e.year]),[[202,'Search movie 202',2021]]);
 await addList('Custom list','movies');
 const mform=panel.getByRole('form',{name:'Add Custom list'});
 await mform.waitFor();
 assert.equal(await mform.getByLabel('Minimum availability',{exact:true}).count(),1);
 assert.equal(await mform.getByLabel('Series type',{exact:true}).count(),0,'tv-only field hidden for movies');
 assert.deepEqual(await mform.getByLabel('Monitor',{exact:true}).locator('option').allTextContents(),['Monitored','Not monitored']);
 assert.deepEqual(await mform.getByRole('group',{name:'Tags',exact:true}).locator('label').allTextContents().then(t=>t.map(x=>x.trim())),['movie-list'],'only movie tags offered');
 await mform.getByLabel('Name',{exact:true}).fill('Movie feed');
 await mform.getByLabel(/^List URL/).fill(`${fixture.list_origin}/feed/movies`);
 await defaults(mform,{profile:'Lists movie profile',root:fixture.movie_path,tag:'movie-list'});
 await mform.getByRole('button',{name:'Save',exact:true}).click();
 await panel.getByText('Import list "Movie feed" saved.').waitFor();
 await panel.getByRole('button',{name:'Sync now Movie feed',exact:true}).click();
 await status('Movie feed').getByText(/^Succeeded: fetched 2, added 1, already in library 0, excluded 1/).waitFor();
 const movies=(await library('movies')).filter(m=>[202,203].includes(m.tmdb_id));
 assert.deepEqual(movies.map(m=>m.tmdb_id),[203],'the excluded movie is skipped and the other added');
 assert.equal(movies[0].settings.quality_profile_id,movieProfile.id);
 const movieList=(await lists()).find(l=>l.name==='Movie feed');
 assert.deepEqual((await items(movieList.id)).map(i=>[i.tmdb_id,i.state]),[[202,'excluded'],[203,'added']]);
 assert.equal(movieList.media_type,'movies');

 // --- Broken feed: edit the TV list; the failure shows the plain-language class and keeps previous state.
 const before=(await lists()).find(l=>l.name==='TV feed');
 await panel.getByRole('button',{name:'Edit TV feed',exact:true}).click();
 const eform=panel.getByRole('form',{name:'Edit TV feed'});
 await eform.waitFor();
 assert.equal(await eform.getByLabel('Bearer token',{exact:true}).inputValue(),'');
 assert.match(await eform.getByLabel('Bearer token',{exact:true}).getAttribute('placeholder'),/Configured \(leave blank to keep\)/);
 await page.screenshot({path:join(artifacts,'import-lists-edit.png'),fullPage:true});
 await eform.getByLabel(/^List URL/).fill(`${fixture.list_origin}/feed/broken`);
 await eform.getByRole('button',{name:'Test',exact:true}).click();
 await panel.getByRole('status',{name:'Test result'}).getByText(/Test failed: The list service answered with data that is not a valid list/).waitFor();
 await eform.getByRole('button',{name:'Save',exact:true}).click();
 await panel.getByText('Import list "TV feed" saved.').waitFor();
 const edited=(await lists()).find(l=>l.name==='TV feed');
 assert.deepEqual(edited.secrets_configured,['bearer_token'],'a blank secret keeps the stored one');
 assert.equal(edited.revision,before.revision+1);
 await panel.getByRole('button',{name:'Sync now TV feed',exact:true}).click();
 await status('TV feed').getByText(/^Failed: The list service answered with data that is not a valid list/).waitFor();
 const failed=(await lists()).find(l=>l.name==='TV feed');
 assert.equal(failed.last_error,'invalid_response');assert.equal(failed.last_success_at,before.last_success_at,'last success is kept');
 await panel.getByText(/Last sync .* failed: The list service answered with data that is not a valid list/).first().waitFor();
 assert.deepEqual((await items(failed.id)).map(i=>i.state),['added','added'],'previous membership is kept');
 assert.equal((await library('tv')).filter(s=>[202,203].includes(s.tvdb_id)).length,2);
 await page.screenshot({path:join(artifacts,'import-lists-failed.png'),fullPage:true});

 // --- Secrets: clear is explicit; the value is never shown.
 await panel.getByRole('button',{name:'Edit TV feed',exact:true}).click();
 await eform.getByLabel('Clear Bearer token',{exact:true}).check();
 await eform.getByRole('button',{name:'Save',exact:true}).click();
 await panel.getByText('Import list "TV feed" saved.').waitFor();
 assert.deepEqual((await lists()).find(l=>l.name==='TV feed').secrets_configured,[]);
 assert.ok(!(await page.content()).includes(TOKEN));

 // --- Sync all covers both enabled lists.
 await panel.getByRole('button',{name:'Sync all',exact:true}).click();
 await panel.getByText(/Sync requested for \d+ lists?;/).waitFor();
 await status('Movie feed').getByText(/^Succeeded: fetched 2, added 0, already in library 0, excluded 1/).waitFor();

 // --- Exclusions: add, filter, single delete, select-many bulk delete.
 for(const [media,id,title] of [['tv','9001','Excluded show A'],['tv','9002','Excluded show B']]){
  await exclusions.getByLabel('Exclusion media type',{exact:true}).selectOption(media);
  await exclusions.getByLabel('TVDB ID',{exact:true}).fill(id);
  await exclusions.getByLabel('Title',{exact:true}).fill(title);
  await exclusions.getByRole('button',{name:'Add exclusion',exact:true}).click();
  await exclusions.getByText(new RegExp(`Excluded "${title}"`)).waitFor();
 }
 await exclusions.getByLabel('Title',{exact:true}).fill('');
 await exclusions.getByLabel('TVDB ID',{exact:true}).fill('0');
 await exclusions.getByLabel('Title',{exact:true}).fill('Bad id');
 await exclusions.getByRole('button',{name:'Add exclusion',exact:true}).click();
 await exclusions.getByRole('alert').getByText(/TVDB ID must be a positive whole number/).waitFor();
 await exclusions.getByLabel('Exclusion media type filter',{exact:true}).selectOption('tv');
 await exclusions.getByText('Excluded show A').waitFor();
 await until(()=>exclusions.getByText('Search movie 202').count(),n=>n===0,'domain filter hides movie exclusions');
 await page.screenshot({path:join(artifacts,'import-lists-exclusions.png'),fullPage:true});
 await exclusions.getByRole('button',{name:'Remove exclusion Excluded show A',exact:true}).click();
 await exclusions.getByText('Exclusion for "Excluded show A" removed.').waitFor();
 assert.deepEqual((await api('get','/api/v1/importlist/exclusions?media_type=tv')).items.map(e=>e.external_id),[9002]);
 await exclusions.getByLabel('Exclusion media type filter',{exact:true}).selectOption('');
 await exclusions.getByLabel('Select all on this page').check();
 await exclusions.getByRole('button',{name:'Delete selected (2)',exact:true}).click();
 await exclusions.getByText('2 exclusions removed.').waitFor();
 await exclusions.getByText('No exclusions.').waitFor();
 assert.equal((await api('get','/api/v1/importlist/exclusions')).total,0);

 // --- Mobile: no horizontal overflow, including the edit form.
 await page.setViewportSize({width:390,height:844});
 await page.reload();await panel.waitFor();
 await panel.getByText('TV feed').first().waitFor();
 await noOverflow('list');
 await page.screenshot({path:join(artifacts,'import-lists-mobile.png'),fullPage:true});
 await panel.getByRole('button',{name:'Edit TV feed',exact:true}).click();
 await panel.getByRole('form',{name:'Edit TV feed'}).waitFor();
 await noOverflow('edit form');
 await page.screenshot({path:join(artifacts,'import-lists-mobile-edit.png'),fullPage:true});
 await panel.getByRole('button',{name:'Cancel',exact:true}).click();
 await page.setViewportSize({width:1280,height:1000});

 // --- Delete both lists; library items they added remain.
 for(const name of ['TV feed','Movie feed']){
  await panel.getByRole('button',{name:`Delete ${name}`,exact:true}).click();
  await panel.getByText(`Import list "${name}" deleted.`).waitFor();
 }
 await panel.getByText('No import lists are configured.').waitFor();
 assert.deepEqual(await lists(),[]);
 assert.equal((await library('tv')).filter(s=>[202,203].includes(s.tvdb_id)).length,2);
 assert.deepEqual(errors,[]);
 console.log('Import lists browser passed: add/validate/test/save, write-only token, sync queued->succeeded added=2 with profile+tag, items view, second sync adds nothing, movie exclusion skip, broken feed keeps state, secret clear, sync all, exclusion single+bulk delete, 390px no overflow, delete.');
}catch(error){await page.screenshot({path:join(artifacts,'import-lists-failure.png'),fullPage:true}).catch(()=>{});throw error;}finally{await browser.close();}
