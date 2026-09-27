// Owned fixture only: UI_URL points to Vite proxying the printed loopback API origin.
import assert from 'node:assert/strict';
import {pathToFileURL} from 'node:url';
const {chromium}=await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE || '/usr/lib/node_modules/playwright/index.mjs').href);
const origin=process.env.UI_URL;assert.ok(origin);assert.equal(new URL(origin).hostname,'127.0.0.1');
const browser=await chromium.launch({headless:true});const page=await browser.newPage();page.setDefaultTimeout(15000);page.on('dialog',d=>d.accept());
const errors=[];page.on('pageerror',e=>errors.push(e.message));
const panel=page.getByRole('region',{name:'Global quality settings',exact:true});const profiles=page.getByRole('region',{name:'Quality profiles',exact:true});
async function read(path){const response=await page.request.get(`${origin}/api/v1/${path}`);assert.equal(response.status(),200);return response.json();}
async function save(){const response=page.waitForResponse(r=>r.url().endsWith('/quality-definitions/bulk') && r.request().method()==='PUT');await panel.getByRole('button',{name:'Save quality settings',exact:true}).click();return response;}
async function reset(titles){await panel.getByRole('button',{name:'Reset quality settings',exact:true}).click();if(titles)await panel.getByLabel('Also reset display titles').check();const response=page.waitForResponse(r=>r.url().endsWith('/quality-definitions/reset'));await panel.getByRole('button',{name:'Confirm quality reset',exact:true}).click();assert.equal((await response).status(),200);await panel.getByText('Quality settings reset.',{exact:true}).waitFor();}
async function editProfile(name) {
 await profiles.getByRole('button',{name:`Edit ${name}`,exact:true}).click();
 await page.waitForFunction(()=>{const fieldset=document.querySelector('section[aria-label="Quality profiles"] form > fieldset');return fieldset && !fieldset.disabled;});
}
const ids={};
try {
 for(const domain of ['tv','movies']) {const response=await page.request.post(`${origin}/api/v1/${domain}/quality-profiles`,{data:{name:`Propagation ${domain}`,policy:null,items:[{kind:'quality',quality_id:1,allowed:true,min_size:10,preferred_size:20,max_size:30},{kind:'quality',quality_id:2,allowed:false,min_size:30,preferred_size:40,max_size:50}]}});assert.equal(response.status(),201);ids[domain]=(await response.json()).id;}
 const originalMovie=await read('movies/quality-definitions');
 await page.goto(origin);await page.getByRole('navigation',{name:'Workspace'}).getByRole('button',{name:'Profiles',exact:true}).click();await editProfile('Propagation tv');
 // Hold the profile write before delivery: the other panel must not overlap a global write.
 let captureProfile;const heldProfile=new Promise(resolve=>captureProfile=resolve);
 const profilePattern=`**/api/v1/tv/quality-profiles/${ids.tv}`;
 await page.route(profilePattern,route=>route.request().method()==='PUT'?captureProfile(route):route.continue());
 await profiles.getByRole('button',{name:'Save profile',exact:true}).click();const profileRoute=await heldProfile;
 await page.getByRole('button',{name:'Quality settings',exact:true}).click();await panel.getByText('Quality profile write in progress.',{exact:false}).waitFor();
 assert.equal(await panel.getByRole('button',{name:'Save quality settings',exact:true}).isDisabled(),true);assert.equal(await panel.getByRole('button',{name:'Reset quality settings',exact:true}).isDisabled(),true);
 await profileRoute.continue();await page.unroute(profilePattern);
 await page.waitForFunction(()=>{const f=document.querySelector('section[aria-label="Global quality settings"] form > fieldset');return f && !f.disabled;});
 await page.getByRole('navigation',{name:'Workspace'}).getByRole('button',{name:'Profiles',exact:true}).click();await editProfile('Propagation tv');await profiles.getByLabel('Profile name',{exact:true}).fill('Keep this draft');
 await page.getByRole('button',{name:'Quality settings',exact:true}).click();
 await panel.getByLabel('SDTV display title',{exact:true}).fill('Global TV');await panel.getByLabel('SDTV minimum size',{exact:true}).fill('11');await panel.getByLabel('SDTV preferred size',{exact:true}).fill('');await panel.getByLabel('SDTV maximum size',{exact:true}).fill('40');
 // Reverse ordering: a held global write blocks the retained profile draft.
 let captureGlobal;const heldGlobal=new Promise(resolve=>captureGlobal=resolve);const globalPattern='**/api/v1/tv/quality-definitions/bulk';
 await page.route(globalPattern,route=>captureGlobal(route));
 const globalResponse=page.waitForResponse(r=>r.url().endsWith('/tv/quality-definitions/bulk'));
 await panel.getByRole('button',{name:'Save quality settings',exact:true}).click();const globalRoute=await heldGlobal;
 await page.getByRole('navigation',{name:'Workspace'}).getByRole('button',{name:'Profiles',exact:true}).click();await profiles.getByText('Global quality write in progress.',{exact:false}).waitFor();
 assert.equal(await profiles.getByRole('button',{name:'Save profile',exact:true}).isDisabled(),true);assert.equal(await profiles.getByLabel('Profile name',{exact:true}).inputValue(),'Keep this draft');
 await globalRoute.continue();assert.equal((await globalResponse).status(),200);await page.unroute(globalPattern);
 let profile=await read(`tv/quality-profiles/${ids.tv}`);assert.deepEqual([profile.items[0].min_size,profile.items[0].preferred_size,profile.items[0].max_size],[11,null,40]);assert.equal(profile.items[1].min_size,30);
 assert.deepEqual(await read('movies/quality-definitions'),originalMovie);
 await page.getByRole('navigation',{name:'Workspace'}).getByRole('button',{name:'Profiles',exact:true}).click();await profiles.getByText('Global quality settings changed.',{exact:false}).waitFor();assert.equal(await profiles.getByRole('button',{name:'Save profile',exact:true}).isDisabled(),true);assert.equal(await profiles.getByLabel('Profile name',{exact:true}).inputValue(),'Keep this draft');
 await profiles.getByRole('button',{name:'Refresh list and catalogs',exact:true}).click();await profiles.getByRole('button',{name:'Edit Propagation tv',exact:true}).waitFor();assert.equal(await profiles.getByRole('button',{name:'Save profile',exact:true}).isDisabled(),true);
 await editProfile('Propagation tv');assert.equal(await profiles.getByLabel('Global TV minimum size',{exact:true}).inputValue(),'11');
 await page.getByRole('button',{name:'Quality settings',exact:true}).click();
 for(const domain of ['tv','movies']) {
  if(domain==='movies')await panel.getByRole('button',{name:'Movies',exact:true}).click();
  await panel.getByLabel('Show catalog defaults').check();
  const before=await read(`${domain}/quality-definitions`);
  // Invalid numeric text remains editable and never produces a partial write.
  await panel.getByLabel('SDTV minimum size',{exact:true}).fill('Infinity');await panel.getByRole('button',{name:'Save quality settings',exact:true}).click();await panel.getByRole('alert').waitFor();assert.deepEqual(await read(`${domain}/quality-definitions`),before);
  await panel.getByLabel('SDTV display title',{exact:true}).fill(`Edited ${domain}`);await panel.getByLabel('SDTV minimum size',{exact:true}).fill('12');await panel.getByLabel('SDTV preferred size',{exact:true}).fill('20');await panel.getByLabel('SDTV maximum size',{exact:true}).fill('40');assert.equal((await save()).status(),200);
  const saved=await read(`${domain}/quality-definitions`);
  await panel.getByRole('button',{name:'Reset quality settings',exact:true}).click();await panel.getByRole('button',{name:'Cancel quality reset',exact:true}).click();assert.deepEqual(await read(`${domain}/quality-definitions`),saved);
  await reset(false);let rows=await read(`${domain}/quality-definitions`),row=rows.find(r=>r.id===1);assert.equal(row.title,`Edited ${domain}`);
  if(domain==='tv')assert.equal(row.min_size,12);else {const expected=(await read('movies/quality-definitions/defaults')).find(r=>r.id===1);assert.deepEqual([row.min_size,row.preferred_size,row.max_size],[expected.min_size,expected.preferred_size,expected.max_size]);}
  await reset(true);row=(await read(`${domain}/quality-definitions`)).find(r=>r.id===1);assert.equal(row.title,'SDTV');
  if(domain==='tv') {
   for(const name of ['minimum','preferred','maximum'])await panel.getByLabel(`SDTV ${name} size`,{exact:true}).fill('');assert.equal((await save()).status(),200);
   const existing=await read(`tv/quality-profiles/${ids.tv}`);assert.equal(existing.items[0].min_size,12);
   // Reconcile TV draft before movie writes, so isolation is observable in the editor.
   await page.getByRole('navigation',{name:'Workspace'}).getByRole('button',{name:'Profiles',exact:true}).click();await editProfile('Propagation tv');await page.getByRole('button',{name:'Quality settings',exact:true}).click();
  } else {
   assert.equal((await read(`movies/quality-profiles/${ids.movies}`)).items[0].min_size,10);
   await page.getByRole('navigation',{name:'Workspace'}).getByRole('button',{name:'Profiles',exact:true}).click();assert.equal(await profiles.getByRole('button',{name:'Save profile',exact:true}).isDisabled(),false);await page.getByRole('button',{name:'Quality settings',exact:true}).click();
  }
  // An ambiguous response retains the draft and blocks save/reset until explicit readback.
  const pattern=`**/api/v1/${domain}/quality-definitions/bulk`;await page.route(pattern,route=>route.abort('failed'));
  await panel.getByLabel('SDTV display title',{exact:true}).fill('Uncertain draft');await panel.getByRole('button',{name:'Save quality settings',exact:true}).click();await panel.getByText('The write outcome is unknown.',{exact:false}).waitFor();assert.equal(await panel.getByRole('button',{name:'Save quality settings',exact:true}).isDisabled(),true);assert.equal(await panel.getByRole('button',{name:'Reset quality settings',exact:true}).isDisabled(),true);assert.equal(await panel.getByLabel('SDTV display title',{exact:true}).inputValue(),'Uncertain draft');
  if(domain==='tv') {
   await page.getByRole('navigation',{name:'Workspace'}).getByRole('button',{name:'Profiles',exact:true}).click();await profiles.getByText('Global quality write outcome unknown.',{exact:false}).waitFor();
   const currentRead=page.waitForResponse(r=>r.url().endsWith(`/tv/quality-profiles/${ids.tv}`) && r.request().method()==='GET');
   await profiles.getByRole('button',{name:'Edit Propagation tv',exact:true}).click();await (await currentRead).finished();
   assert.equal(await profiles.getByRole('button',{name:'Save profile',exact:true}).isDisabled(),true);
   await page.getByRole('button',{name:'Quality settings',exact:true}).click();
  }
  await page.unroute(pattern);await panel.getByRole('button',{name:'Reload saved settings',exact:true}).click();await panel.getByRole('button',{name:'Save quality settings',exact:true}).waitFor();
  if(domain==='tv'){await page.getByRole('navigation',{name:'Workspace'}).getByRole('button',{name:'Profiles',exact:true}).click();await editProfile('Propagation tv');await page.getByRole('button',{name:'Quality settings',exact:true}).click();}
 }
 assert.deepEqual(errors,[]);console.log('Global quality settings browser workflow passed for TV and movies.');
}finally{await browser.close();}
