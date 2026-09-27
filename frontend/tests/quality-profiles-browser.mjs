// Run against an owned library_ui_fixture with profile schema/delete routes and the Vite proxy.
// UI_URL must be the printed loopback Vite URL. No real providers or media are used.
import assert from 'node:assert/strict';
import {pathToFileURL} from 'node:url';
const {chromium}=await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE || '/usr/lib/node_modules/playwright/index.mjs').href);
const origin=process.env.UI_URL;assert.ok(origin);assert.equal(new URL(origin).hostname,'127.0.0.1');
const browser=await chromium.launch({headless:true});
const page=await browser.newPage();page.setDefaultTimeout(15000);
const roots={tv:process.env.UI_TV_ROOT,movies:process.env.UI_MOVIE_ROOT};assert.ok(roots.tv && roots.movies,'Use roots printed by the owned fixture');
const errors=[];page.on('pageerror',e=>errors.push(e.message));page.on('dialog',d=>d.accept());
const panel=page.getByRole('region',{name:'Quality profiles',exact:true});
async function read(path) {const r=await page.request.get(`${origin}/api/v1/${path}`);assert.equal(r.status(),200);return r.json();}
try {
 await page.goto(origin);await page.getByRole('button',{name:'Quality profiles',exact:true}).click();
 for(const domain of ['tv','movies']) {
  if(domain==='movies') await panel.getByRole('button',{name:'Movies',exact:true}).click();
  for(const name of ['Browser positive','Browser zero']) {
   const response=await page.request.post(`${origin}/api/v1/${domain}/custom-formats`,{data:{name,include_when_renaming:false,specifications:[{name:'English',negate:false,required:false,condition:{kind:'language',value:1,except_language:false}}]}});assert.equal(response.status(),201);
  }
  await panel.getByRole('button',{name:'Refresh list and catalogs',exact:true}).click();
  await panel.getByRole('button',{name:'New profile',exact:true}).click();
  const name=`Browser profile ${domain}`;
  await panel.getByLabel('Profile name',{exact:true}).fill(name);
  await panel.getByLabel('Allow SDTV',{exact:true}).check();await panel.getByLabel('Allow DVD',{exact:true}).check();
  await panel.getByLabel('New group name').fill('Browser group');await panel.getByRole('button',{name:'Create group',exact:true}).click();
  await panel.getByLabel('Move SDTV to',{exact:true}).selectOption({label:'Browser group'});
  await panel.getByLabel('Move DVD to',{exact:true}).selectOption({label:'Browser group'});
  await panel.getByLabel('Allow SDTV',{exact:true}).uncheck();await panel.getByLabel('Allow DVD',{exact:true}).uncheck();
  assert.equal(await panel.getByLabel('Allow Browser group',{exact:true}).isChecked(),false);
  await panel.getByLabel('Allow SDTV',{exact:true}).check();assert.equal(await panel.getByLabel('Allow Browser group',{exact:true}).isChecked(),true);
  await panel.getByLabel('Allow DVD',{exact:true}).check();
  await panel.getByLabel('Quality cutoff',{exact:true}).selectOption({label:'Browser group'});
  await panel.getByRole('button',{name:'Move Browser group up',exact:true}).click();
  await panel.getByRole('button',{name:'Move DVD up in group',exact:true}).click();
  await panel.getByLabel('Allow upgrades',{exact:true}).check();
  await panel.getByLabel('Browser positive score',{exact:true}).fill('25');
  if(domain==='movies') await panel.getByLabel('Movie language').selectOption({label:'Original'});
  const savedResponse=page.waitForResponse(r=>r.url().endsWith(`/api/v1/${domain}/quality-profiles`) && r.request().method()==='POST');
  await panel.getByRole('button',{name:'Save profile',exact:true}).click();
  const response=await savedResponse;assert.equal(response.status(),201);const created=await response.json();
  const saved=await read(`${domain}/quality-profiles/${created.id}`);
  assert.equal(saved.name,name);assert.equal(saved.policy.upgrade_allowed,true);
  assert.equal(saved.policy.cutoff.kind,'group');const group=saved.items[saved.policy.cutoff.position];
  assert.equal(group.name,'Browser group');assert.deepEqual(group.items.map(l=>l.quality_id),[2,1]);
  const schema=await read(`${domain}/quality-profiles/schema`);
  const leaves=items=>items.flatMap(i=>i.kind==='group'?i.items:[i]).map(l=>l.quality_id).sort((a,b)=>a-b);
  assert.deepEqual(leaves(saved.items),leaves(schema.items));
  assert.equal(saved.policy.language_id,domain==='movies'?-2:null);
  assert.deepEqual(saved.policy.format_items.map(f=>f.score).sort((a,b)=>a-b),[0,25]);
  // Workspace navigation retains the current draft and stable group selection.
  await panel.getByLabel('Profile name',{exact:true}).fill(`${name} draft`);
  await page.getByRole('button',{name:'Library',exact:true}).click();await page.getByRole('button',{name:'Quality profiles',exact:true}).click();
  assert.equal(await panel.getByLabel('Profile name',{exact:true}).inputValue(),`${name} draft`);
  // A lost write response blocks retry and preserves input until explicit reconciliation.
  await page.route(`**/api/v1/${domain}/quality-profiles/${created.id}`,route=>route.request().method()==='PUT'?route.abort('failed'):route.continue());
  await panel.getByRole('button',{name:'Save profile',exact:true}).click();await panel.getByText('The write outcome is unknown.',{exact:false}).waitFor();
  assert.equal(await panel.getByRole('button',{name:'Save profile',exact:true}).isDisabled(),true);
  assert.equal(await panel.getByLabel('Profile name',{exact:true}).inputValue(),`${name} draft`);
  await page.unroute(`**/api/v1/${domain}/quality-profiles/${created.id}`);
  await panel.getByRole('button',{name:`Edit ${name}`,exact:true}).click();
  await panel.getByRole('button',{name:`Duplicate ${name}`,exact:true}).click();
  const duplicateResponse=page.waitForResponse(r=>r.url().endsWith(`/api/v1/${domain}/quality-profiles`) && r.request().method()==='POST');
  await panel.getByRole('button',{name:'Save profile',exact:true}).click();const copied=await duplicateResponse;assert.equal(copied.status(),201);const duplicate=await copied.json();
  assert.deepEqual(duplicate.items,saved.items);assert.deepEqual(duplicate.policy,saved.policy);
  await panel.getByRole('button',{name:'Delete profile',exact:true}).click();await panel.getByRole('button',{name:'Confirm delete profile',exact:true}).click();
  await panel.getByText('Profile deleted.',{exact:true}).waitFor();
  assert.equal((await page.request.get(`${origin}/api/v1/${domain}/quality-profiles/${duplicate.id}`)).status(),404);
  // Real backend assignment protects deletion; restoring it leaves fixture ownership intact.
  const libraryPath=domain==='tv'?'tv/series':'movies';const library=await read(libraryPath);let item=library.items[0];
  if(!item) { const added=await page.request.post(`${origin}/api/v1/${libraryPath}`,{data:{title:'Profile assignment fixture',path:roots[domain],[domain==='tv'?'tvdb_id':'tmdb_id']:701}});assert.equal(added.status(),201);item=await added.json(); }
  const detail=await read(`${libraryPath}/${item.id}`);
  const assigned=await page.request.put(`${origin}/api/v1/${libraryPath}/${item.id}`,{data:{quality_profile_id:created.id}});assert.equal(assigned.status(),200);
  await panel.getByRole('button',{name:`Edit ${name}`,exact:true}).click();await panel.getByRole('button',{name:'Delete profile',exact:true}).click();
  const rejected=page.waitForResponse(r=>r.request().method()==='DELETE' && r.url().endsWith(`/quality-profiles/${created.id}`));
  await panel.getByRole('button',{name:'Confirm delete profile',exact:true}).click();assert.equal((await rejected).status(),409);
  assert.equal((await read(`${domain}/quality-profiles/${created.id}`)).name,name);
  assert.equal((await page.request.put(`${origin}/api/v1/${libraryPath}/${item.id}`,{data:{quality_profile_id:detail.settings.quality_profile_id}})).status(),200);
 }
 assert.deepEqual(errors,[]);console.log('Quality profile browser workflow passed for TV and movies.');
} finally {await browser.close();}
