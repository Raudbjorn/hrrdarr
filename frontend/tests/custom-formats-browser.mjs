// Run with UI_URL pointing to Vite backed by your fresh owned library_ui_fixture.
import assert from 'node:assert/strict';
import { pathToFileURL } from 'node:url';
const {chromium}=await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE || '/usr/lib/node_modules/playwright/index.mjs').href);
const origin=process.env.UI_URL;
assert.equal(new URL(origin).hostname,'127.0.0.1');
const browser=await chromium.launch({headless:true});
try {
  const page=await browser.newPage();page.setDefaultTimeout(10000);
  const errors=[];page.on('pageerror',e=>{errors.push(e.message);console.error('Browser error:',e.message);});
  await page.goto(origin);
  await page.getByRole('button',{name:'Custom formats',exact:true}).click();
  const panel=page.locator('section').filter({has:page.getByRole('heading',{name:'Custom formats',exact:true})});
  for(const [media,button] of [['tv','TV'],['movies','Movies']]) {
    await panel.getByRole('button',{name:button,exact:true}).click();
    await panel.getByText('No custom formats in this library.').waitFor();
    await panel.getByRole('button',{name:'New custom format',exact:true}).click();
    await panel.getByLabel('Format name',{exact:true}).fill(`Preset ${media}`);
    await panel.getByLabel('Condition preset').selectOption({label:'x265'});
    await panel.getByLabel('Condition name',{exact:true}).fill('Codec');
    await panel.getByLabel('Negate',{exact:true}).check();
    await panel.getByLabel('Required',{exact:true}).check();
    await panel.getByLabel('Pattern',{exact:true}).fill('(');
    await panel.getByRole('button',{name:'Save custom format',exact:true}).click();
    await panel.getByRole('alert').waitFor();
    assert.equal(await panel.getByLabel('Pattern',{exact:true}).inputValue(),'(');
    await panel.getByLabel('Pattern',{exact:true}).fill('(?<=WEB[.-])HEVC');
    await panel.getByRole('button',{name:'Save custom format',exact:true}).click();
    await panel.getByText('Custom format saved.',{exact:true}).waitFor();
    let stored=await (await page.request.get(`${origin}/api/v1/${media}/custom-formats`)).json();
    assert.equal(stored.length,1);assert.equal(stored[0].specifications[0].negate,true);assert.equal(stored[0].specifications[0].required,true);
    const id=stored[0].id;
    await panel.getByLabel('Include when renaming',{exact:true}).check();
    // Workspace navigation must retain ownership of a pending write and its draft.
    const updatePattern=`**/api/v1/${media}/custom-formats/${id}`;
    let capture;const held=new Promise(resolve=>capture=resolve);
    await page.route(updatePattern,route=>route.request().method()==='PUT'?capture(route):route.continue());
    const updateResponse=page.waitForResponse(r=>r.url().endsWith(`/custom-formats/${id}`) && r.request().method()==='PUT');
    // Closing a failed held-request assertion must not mask it with an unhandled waiter rejection.
    updateResponse.catch(()=>{});
    await panel.getByRole('button',{name:'Save custom format',exact:true}).click();const heldRoute=await held;
    await page.getByRole('button',{name:'Library',exact:true}).click();await page.getByRole('button',{name:'Custom formats',exact:true}).click();
    assert.equal(await panel.getByRole('button',{name:'Save custom format',exact:true}).count(),1);
    assert.equal(await panel.getByRole('button',{name:'Save custom format',exact:true}).isDisabled(),true);
    assert.equal(await panel.getByLabel('Format name',{exact:true}).inputValue(),`Preset ${media}`);
    assert.equal(await panel.getByLabel('Include when renaming',{exact:true}).isChecked(),true);
    await heldRoute.continue();assert.equal((await updateResponse).status(),200);await page.unroute(updatePattern);
    await panel.getByText('Custom format saved.',{exact:true}).waitFor();
    // An unknown outcome also survives leaving and reopening the workspace.
    await panel.getByLabel('Pattern',{exact:true}).fill('uncertain-workspace-draft');
    await page.route(updatePattern,route=>route.request().method()==='PUT'?route.abort('failed'):route.continue());
    await panel.getByRole('button',{name:'Save custom format',exact:true}).click();await panel.getByText('The outcome is uncertain.',{exact:false}).waitFor();
    await page.getByRole('button',{name:'Library',exact:true}).click();await page.getByRole('button',{name:'Custom formats',exact:true}).click();
    assert.equal(await panel.getByLabel('Pattern',{exact:true}).inputValue(),'uncertain-workspace-draft');
    assert.equal(await panel.getByRole('button',{name:'Save custom format',exact:true}).isDisabled(),true);
    await page.unroute(updatePattern);await panel.getByRole('button',{name:'Reload custom formats',exact:true}).click();
    await panel.getByRole('button',{name:`Preset ${media}`,exact:true}).click();
    assert.equal((await (await page.request.get(`${origin}/api/v1/${media}/custom-formats/${id}`)).json()).include_when_renaming,true);

    await panel.getByRole('button',{name:'New custom format',exact:true}).click();
    await panel.getByLabel('Format name',{exact:true}).fill(`Clone ${media}`);
    await panel.getByLabel('Condition preset').selectOption({label:`Preset ${media}: Codec`});
    assert.equal(await panel.getByLabel('Pattern',{exact:true}).inputValue(),'(?<=WEB[.-])HEVC');
    await panel.getByLabel('Pattern',{exact:true}).fill('different');
    await panel.getByRole('button',{name:'Save custom format',exact:true}).click();
    await panel.getByText('Custom format saved.',{exact:true}).waitFor();
    stored=await (await page.request.get(`${origin}/api/v1/${media}/custom-formats`)).json();
    assert.equal(stored.find(f=>f.id===id).specifications[0].condition.pattern,'(?<=WEB[.-])HEVC');
    await panel.getByRole('button',{name:'Delete custom format',exact:true}).click();
    await panel.getByRole('button',{name:'Cancel deletion',exact:true}).click();
    assert.equal((await (await page.request.get(`${origin}/api/v1/${media}/custom-formats`)).json()).length,2);
    await panel.getByRole('button',{name:'Delete custom format',exact:true}).click();
    await panel.getByRole('button',{name:'Confirm delete custom format',exact:true}).click();
    await panel.getByText('Custom format deleted.',{exact:true}).waitFor();
    await panel.getByRole('button',{name:`Preset ${media}`,exact:true}).click();
    assert.equal(await panel.getByLabel('Pattern',{exact:true}).inputValue(),'(?<=WEB[.-])HEVC');
  }
  // Each typed parameter family can be edited and persisted, including movie-only kinds.
  await panel.getByRole('button',{name:'Preset movies',exact:true}).click();
  const schema=await (await page.request.get(`${origin}/api/v1/movies/custom-formats/schema`)).json();
  for(const condition of schema.conditions.filter(c=>c.kind!=='release_title')) {
    await panel.getByLabel('Condition kind',{exact:true}).selectOption(condition.kind);
    await panel.getByRole('button',{name:'Add condition',exact:true}).click();
  }
  await panel.getByLabel('Except language',{exact:true}).check();
  await panel.getByLabel('Minimum GiB',{exact:true}).fill('1');
  await panel.getByLabel('Maximum GiB',{exact:true}).fill('10');
  await panel.getByLabel('Minimum year',{exact:true}).fill('2000');
  await panel.getByLabel('Maximum year',{exact:true}).fill('2030');
  await panel.getByRole('button',{name:'Save custom format',exact:true}).click();
  await panel.getByText('Custom format saved.',{exact:true}).waitFor();
  const movie=await (await page.request.get(`${origin}/api/v1/movies/custom-formats`)).json();
  assert.equal(movie[0].specifications.length,schema.conditions.length);
  assert.deepEqual(movie[0].specifications.find(s=>s.condition.kind==='size').condition,{kind:'size',min_gib:1,max_gib:10});
  assert.equal(movie[0].specifications.find(s=>s.condition.kind==='language').condition.except_language,true);
  await panel.getByRole('button',{name:'Clone condition',exact:true}).first().click();
  await panel.getByText('Import or export JSON',{exact:true}).click();
  await panel.getByRole('button',{name:'Export draft as JSON',exact:true}).click();
  const exported=JSON.parse(await panel.getByLabel('Custom format JSON',{exact:true}).inputValue());
  assert.equal(exported.specifications.length,schema.conditions.length+1);
  assert.deepEqual(exported.specifications[0],exported.specifications[1]);
  await panel.getByRole('button',{name:'Clone format',exact:true}).click();
  assert.equal(await panel.getByRole('button',{name:'Delete custom format',exact:true}).count(),0);
  exported.name='Imported clone';
  await panel.getByLabel('Custom format JSON',{exact:true}).fill('[');
  await panel.getByRole('button',{name:'Apply JSON to new draft',exact:true}).click();
  await panel.getByText('Invalid JSON.',{exact:true}).waitFor();
  await panel.getByLabel('Custom format JSON',{exact:true}).fill(JSON.stringify(exported));
  await panel.getByRole('button',{name:'Apply JSON to new draft',exact:true}).click();
  assert.equal((await (await page.request.get(`${origin}/api/v1/movies/custom-formats`)).json()).length,1);
  await panel.getByRole('button',{name:'Save custom format',exact:true}).click();
  await panel.getByText('Custom format saved.',{exact:true}).waitFor();
  const imported=await (await page.request.get(`${origin}/api/v1/movies/custom-formats`)).json();
  assert.deepEqual(imported.find(f=>f.name==='Imported clone').specifications,exported.specifications);
  await panel.getByLabel('Condition kind',{exact:true}).selectOption('edition');
  await panel.getByRole('button',{name:'TV',exact:true}).click();
  await panel.getByRole('button',{name:'Preset tv',exact:true}).click();
  assert.equal(await panel.getByLabel('Condition kind',{exact:true}).inputValue(),'release_title');
  // Long labels are display values, not invalid >100-character copied condition names.
  const long='界'.repeat(100);
  const created=await page.request.post(`${origin}/api/v1/tv/custom-formats`,{data:{name:long,include_when_renaming:false,specifications:[{name:long,negate:false,required:false,condition:{kind:'release_title',pattern:'long'}}]}});
  assert.equal(created.status(),201);
  await panel.getByRole('button',{name:'Reload custom formats',exact:true}).click();
  await panel.getByRole('button',{name:'New custom format',exact:true}).click();
  await panel.getByLabel('Format name',{exact:true}).fill('Long clone');
  await panel.getByLabel('Condition preset',{exact:true}).selectOption({label:`${long}: ${long}`});
  assert.equal(await panel.getByLabel('Condition name',{exact:true}).inputValue(),long);
  await panel.getByRole('button',{name:'Save custom format',exact:true}).click();
  await panel.getByText('Custom format saved.',{exact:true}).waitFor();
  // A successful mutation with a failed refresh must keep controls blocked until it settles.
  let refreshStarted, releaseRefresh;
  const started=new Promise(resolve=>refreshStarted=resolve), refresh=new Promise(resolve=>releaseRefresh=resolve);
  await page.route('**/api/v1/tv/custom-formats/schema',async route=>{refreshStarted();await refresh;await route.fulfill({status:503,contentType:'application/json',body:JSON.stringify({error:{code:'refresh',message:'refresh unavailable'}})});});
  await panel.getByLabel('Format name',{exact:true}).fill('Long clone updated');
  await panel.getByRole('button',{name:'Save custom format',exact:true}).click();
  let refreshTimer;
  try {
    await Promise.race([started,new Promise((_,reject)=>{refreshTimer=setTimeout(()=>reject(new Error('Refresh request was not observed')),10000);})]);
  } finally {clearTimeout(refreshTimer);}
  assert.equal(await panel.getByRole('button',{name:'Movies',exact:true}).isDisabled(),true);
  assert.equal(await panel.getByRole('button',{name:'Reload custom formats',exact:true}).isDisabled(),true);
  releaseRefresh();
  await panel.getByText('Saved, but refreshing failed. Reload to continue.',{exact:true}).waitFor();
  await page.unrouteAll({behavior:'wait'});
  await panel.getByRole('button',{name:'Reload custom formats',exact:true}).click();
  await panel.getByRole('button',{name:'Long clone updated',exact:true}).waitFor();
  await panel.getByRole('button',{name:'Movies',exact:true}).click();
  await panel.getByRole('button',{name:'Preset movies',exact:true}).waitFor();
  // A delayed TV response must not replace the newer movie selection.
  let release; const held=new Promise(resolve=>release=resolve);
  await page.route('**/api/v1/tv/custom-formats/schema',async route=>{await held;await route.fulfill({status:503,contentType:'application/json',body:JSON.stringify({error:{code:'delayed',message:'obsolete TV failure'}})});});
  await panel.getByRole('button',{name:'TV',exact:true}).click();
  await panel.getByRole('button',{name:'Movies',exact:true}).click();
  await panel.getByRole('button',{name:'TV',exact:true}).click();
  await panel.getByRole('button',{name:'Movies',exact:true}).click();
  await panel.getByRole('button',{name:'Preset movies',exact:true}).waitFor();
  release();await page.unrouteAll({behavior:'wait'});
  assert.equal(await panel.getByText('obsolete TV failure').count(),0);
  assert.equal(await panel.getByRole('button',{name:'Preset tv',exact:true}).count(),0);
  for(const [media,button] of [['tv','TV'],['movies','Movies']]) {
    await panel.getByRole('button',{name:button,exact:true}).click();
    await panel.getByRole('button',{name:`Preset ${media}`,exact:true}).click();
    await panel.getByText('Import or export JSON',{exact:true}).click();
    await panel.getByLabel('JSON format',{exact:true}).selectOption('community');
    const before=(await (await page.request.get(`${origin}/api/v1/${media}/custom-formats`)).json()).length;
    const community={id:123,media_type:media==='tv'?'movies':'tv',domain:'spoofed',trash_id:'private-trash-id',trash_scores:{default:10000},notes:{arbitrary:'private note'},name:`Community ${media}`,includeCustomFormatWhenRenaming:null,specifications:[{id:42,trash_id:'private-spec-id',notes:['private note'],domain:'spoofed',future:true,implementationName:'private presentation',infoLink:'https://private.invalid',name:'Imported title',implementation:'ReleaseTitleSpecification',negate:true,required:true,fields:{value:'community'}}]};
    await panel.getByLabel('Custom format JSON',{exact:true}).fill(JSON.stringify(community));
    await panel.getByRole('button',{name:'Apply JSON to new draft',exact:true}).click();
    assert.equal((await (await page.request.get(`${origin}/api/v1/${media}/custom-formats`)).json()).length,before);
    assert.equal(await panel.getByLabel('Format name',{exact:true}).inputValue(),`Community ${media}`);
    await panel.getByLabel('Pattern',{exact:true}).fill('edited-community');
    await panel.getByRole('button',{name:'Export draft as JSON',exact:true}).click();
    const output=JSON.parse(await panel.getByLabel('Custom format JSON',{exact:true}).inputValue());
    assert.deepEqual(output,{name:`Community ${media}`,includeCustomFormatWhenRenaming:false,specifications:[{name:'Imported title',implementation:'ReleaseTitleSpecification',negate:true,required:true,fields:{value:'edited-community'}}]});
    const invalid={...community,specifications:[...community.specifications,{name:'Future',implementation:'FutureSpecification',fields:{value:1}}]};
    await panel.getByLabel('Custom format JSON',{exact:true}).fill(JSON.stringify(invalid));
    await panel.getByRole('button',{name:'Apply JSON to new draft',exact:true}).click();
    await panel.getByText('Unknown community condition or condition fields.',{exact:true}).waitFor();
    assert.equal(await panel.getByLabel('Pattern',{exact:true}).inputValue(),'edited-community');
    assert.equal((await (await page.request.get(`${origin}/api/v1/${media}/custom-formats`)).json()).length,before);
    await panel.getByRole('button',{name:'Save custom format',exact:true}).click();
    await panel.getByText('Custom format saved.',{exact:true}).waitFor();
    const saved=(await (await page.request.get(`${origin}/api/v1/${media}/custom-formats`)).json()).find(f=>f.name===`Community ${media}`);
    assert.equal(saved.include_when_renaming,false);assert.equal(saved.media_type,media);assert.notEqual(saved.id,123);assert.equal(JSON.stringify(saved).includes('private'),false);assert.equal(JSON.stringify(saved).includes('trash_'),false);
    assert.equal(saved.specifications[0].condition.pattern,'edited-community');assert.equal(saved.specifications[0].required,true);assert.equal(saved.specifications[0].negate,true);
  }
  assert.deepEqual(errors,[]);
  console.log('PASS custom format browser: both-domain preset create/edit/copy/delete, validation, typed fields, native/community JSON/clone, long labels, failed refresh, persisted isolation, stale response');
} finally {await browser.close();}
