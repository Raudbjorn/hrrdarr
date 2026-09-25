// Opt-in real browser regression. Requires the owned library_ui_fixture and Vite proxy.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';
const { chromium } = await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE || '/usr/lib/node_modules/playwright/index.mjs').href);
const origin = process.env.UI_URL;
const scratch = process.env.UI_SCRATCH;
assert.ok(origin && scratch, 'Set UI_URL and UI_SCRATCH from your owned fixture');
assert.equal(new URL(origin).hostname, '127.0.0.1');
const browser = await chromium.launch({ headless: true });
const page = await browser.newPage();
page.setDefaultTimeout(10000);
const errors = [];
page.on('pageerror', error => errors.push(error.message));
async function add(domain, title, path) {
  await page.getByRole('button', {name: domain === 'tv' ? 'TV' : 'Movies', exact:true}).click();
  await page.getByRole('button', {name: domain === 'tv' ? 'Add series' : 'Add movie', exact:true}).click();
  await page.getByLabel('Search catalogue').fill('Fixture');
  await page.getByRole('button', {name:'Search', exact:true}).click();
  await page.getByRole('button', {name:new RegExp(title+' .*')} ).click();
  await page.getByLabel('Existing library directory').fill(path);
  await page.getByRole('button', {name:'Add to library', exact:true}).click();
  await page.getByRole('article').getByRole('heading', {name:title, exact:true}).waitFor();
}
async function prepare(source, destination) {
  await page.getByLabel('Source file', {exact:true}).fill(source);
  await page.getByLabel('Destination file', {exact:true}).fill(destination);
  await page.getByRole('button', {name:'Preview import', exact:true}).click();
  await page.getByRole('button', {name:'Execute import', exact:true}).waitFor();
}
const receipt = () => page.evaluate(() => JSON.parse(localStorage.getItem('hrrdarr.import.receipt.v1')));
async function verifyProviders() {
  const providerOrigin=process.env.UI_PROVIDER_ORIGIN;
  assert.ok(providerOrigin,'Set UI_PROVIDER_ORIGIN from the owned fixture');
  assert.equal(new URL(providerOrigin).hostname,'127.0.0.1');
  await page.getByRole('button',{name:'Providers',exact:true}).click();
  const editor=page.getByRole('article',{name:'Provider editor'});
  const saved=async(name)=>{const response=await page.request.get(`${origin}/api/v1/providers`);assert.equal(response.status(),200);return (await response.json()).items.find(p=>p.name===name);};
  const save=async()=>{await page.getByRole('button',{name:'Save provider',exact:true}).click();await page.getByRole('button',{name:'Test saved provider',exact:true}).waitFor();await page.waitForFunction(()=>!Array.from(document.querySelectorAll('button')).find(b=>b.textContent==='Test saved provider')?.disabled);};
  const credentials=async(type,value)=>{await page.getByLabel('Credential action').selectOption('replace');if(type==='qbittorrent'){await page.getByLabel('Username',{exact:true}).fill('fixture-user');await page.getByLabel('Password',{exact:true}).fill(value);}else{await page.getByLabel('API key',{exact:true}).fill(value);}};
  const tested=async(success)=>{await page.getByRole('button',{name:'Test saved provider',exact:true}).click();await editor.getByText(new RegExp('Saved test: '+(success?'success':'failure'))).waitFor();if(success)await editor.getByText(/Tested revision .*tv, movies/).waitFor();};
  for(const type of ['torznab','newznab','qbittorrent']) {
    const name=`Browser ${type}`;
    await page.getByRole('button',{name:'New provider',exact:true}).click();
    await page.getByLabel('Provider type').selectOption(type);
    await page.getByLabel('Name',{exact:true}).fill(name);
    await page.getByLabel('Endpoint',{exact:true}).fill(type==='qbittorrent'?providerOrigin:`${providerOrigin}/${type}`);
    await page.getByLabel('TV scope',{exact:true}).check();await page.getByLabel('Movie scope',{exact:true}).check();
    if(type==='qbittorrent'){await page.getByLabel('TV download category').fill('tv');await page.getByLabel('Movie download category').fill('movies');}
    else{await page.getByLabel('TV categories',{exact:true}).fill('5030');await page.getByLabel('Anime categories',{exact:true}).fill('5070');await page.getByLabel('Movie categories',{exact:true}).fill('2000');}
    await credentials(type,'fixture-bad');await save();await tested(false);
    assert.equal(await editor.locator('input[type=password]').count(),0,'Saved secrets must be cleared');
    await credentials(type,'fixture-good');await save();await tested(true);
    const before=await saved(name);assert.equal(before.has_credentials,true);
    // Independent real API writer adds hidden settings/private bundle to this UI-created record.
    const update={revision:before.revision,name:before.name,enabled:before.enabled,priority:before.priority,settings:before.settings};
    if(type==='qbittorrent'){update.settings.tv.initial_state='forced';update.settings.movies.content_layout='subfolder';}
    else{update.settings.tv.anime_standard_format_search=true;update.settings.movies.remove_year=true;update.credentials={kind:'indexer',api_key:'fixture-good',tv_parameters:[{name:'private_tv',value:'fixture-tv'}],movie_parameters:[{name:'private_movie',value:'fixture-movie'}]};}
    let response=await page.request.put(`${origin}/api/v1/providers/${before.id}`,{data:update});assert.equal(response.status(),200,await response.text());
    await page.getByLabel('Priority',{exact:true}).fill('2');
    await page.getByRole('button',{name:'Save provider',exact:true}).click();
    await page.getByRole('alert').filter({hasText:/changed|revision|conflict/i}).waitFor();
    assert.equal(await page.getByRole('button',{name:'Save provider',exact:true}).isDisabled(),true);
    await page.getByRole('button',{name:'Reload saved provider',exact:true}).click();
    await page.getByLabel('Priority',{exact:true}).fill('3');await save();await tested(true);
    const retained=await saved(name);assert.equal(retained.priority,3);assert.deepEqual(retained.settings,update.settings);assert.equal(retained.has_credentials,true);
    if(type!=='qbittorrent') {
      const observations=await (await page.request.get(`${providerOrigin}/fixture-observations`)).json();
      assert.ok(observations.some(v=>v.path===`/${type}`&&v.category==='5030,5070'&&v.private_tv&&!v.private_movie));
      assert.ok(observations.some(v=>v.path===`/${type}`&&v.category==='2000'&&v.private_movie&&!v.private_tv));
    }
    await page.getByLabel('Credential action').selectOption('clear');await save();assert.equal((await saved(name)).has_credentials,false);await tested(false);
    await credentials(type,'fixture-replacement');await save();await tested(true);
    assert.equal((await saved(name)).has_credentials,true);
    assert.doesNotMatch(await page.locator('body').innerText(),/fixture-(good|replacement|bad)|PRIVATE_FIXTURE/);
    assert.doesNotMatch(await page.evaluate(()=>JSON.stringify(localStorage)),/fixture-(good|replacement|bad)|private_tv|private_movie/);
    await page.reload();await page.getByRole('button',{name:'Providers',exact:true}).click();
    await page.getByRole('button',{name:new RegExp(name+' ')}).click();
    await editor.getByText(/Saved test: success/).waitFor();
    assert.equal(await editor.locator('input[type=password]').count(),0);
    assert.equal(await page.getByLabel('Priority',{exact:true}).inputValue(),'3');
  }
}
try {
  await page.goto(origin);
  await add('tv', 'Fixture series', `${scratch}/tv`);
  await page.getByRole('button', {name:'Unmonitor series', exact:true}).click();
  await page.getByRole('button', {name:'Monitor series', exact:true}).waitFor();
  await page.getByRole('button', {name:'Season 1: monitored', exact:true}).click();
  await page.getByRole('button', {name:'Season 1: unmonitored', exact:true}).waitFor();
  await page.getByRole('button', {name:'Monitor Pilot', exact:true}).click();
  await page.getByRole('button', {name:'Unmonitor Pilot', exact:true}).waitFor();
  await page.getByRole('button', {name:'Import Pilot', exact:true}).click();
  await prepare(`${scratch}/incoming/episode.mkv`, `${scratch}/tv/pilot.mkv`);
  await page.getByRole('button', {name:'Edit unexecuted preview', exact:true}).click();
  await page.getByRole('button', {name:'Preview import', exact:true}).click();
  await page.getByRole('button', {name:'Execute import', exact:true}).waitFor();
  const confirmed = await receipt();
  assert.equal(confirmed.request.target.media_type, 'episode');
  await page.reload();
  await page.getByRole('button', {name:'Execute import', exact:true}).waitFor();
  assert.equal((await receipt()).operation.id, confirmed.operation.id);
  let executions = 0;
  await page.route('**/api/v1/imports/*/execute', async route => {
    executions++;
    const response = await route.fetch(); // Real handler completes; only the browser response is lost.
    assert.equal(response.status(), 200);
    await route.abort('failed');
  });
  await page.getByRole('button', {name:'Execute import', exact:true}).click();
  await page.getByRole('button', {name:'Prepare another import', exact:true}).waitFor();
  assert.equal(executions, 1, 'Lost response must reconcile, never submit another execution automatically');
  assert.equal((await receipt()).operation.id, confirmed.operation.id);
  assert.equal((await receipt()).operation.status, 'complete');
  await page.unroute('**/api/v1/imports/*/execute');
  assert.equal(await readFile(`${scratch}/tv/pilot.mkv`, 'utf8'), 'scratch episode media');
  assert.equal(await readFile(`${scratch}/incoming/episode.mkv`, 'utf8'), 'scratch episode media');
  await page.getByRole('button', {name:'Prepare another import', exact:true}).click();
  // A late TV lookup must not render in the movie scope.
  await page.getByRole('button', {name:'Add series', exact:true}).click();
  await page.getByLabel('Search catalogue').fill('slow');
  await page.getByRole('button', {name:'Search', exact:true}).click();
  await page.getByRole('button', {name:'Movies', exact:true}).click();
  await page.waitForTimeout(2200); // Owned fixture deliberately delays this request by two seconds.
  assert.equal(await page.locator('.lookup-results button').count(), 0);
  await add('movies', 'Fixture movie', `${scratch}/movies`);
  await page.getByRole('button', {name:'Unmonitor movie', exact:true}).click();
  await page.getByRole('button', {name:'Monitor movie', exact:true}).waitFor();
  await prepare(`${scratch}/incoming/movie.mkv`, `${scratch}/movies/movie.mkv`);
  const movieReceipt = await receipt();
  assert.equal(movieReceipt.request.target.media_type, 'movie');
  assert.equal(movieReceipt.request.target.id, confirmed.request.target.id);
  // Simulate a request lost before delivery: retain the same preview for explicit resume.
  await page.route('**/api/v1/imports/*/execute', route => route.abort('failed'));
  await page.getByRole('button', {name:'Execute import', exact:true}).click();
  await page.getByRole('button', {name:'Resume import', exact:true}).waitFor();
  await page.waitForFunction(() => !document.querySelector('.import-panel button')?.disabled);
  assert.equal((await receipt()).operation.id, movieReceipt.operation.id);
  await page.unroute('**/api/v1/imports/*/execute');
  await page.reload();
  await page.getByRole('button', {name:'Resume import', exact:true}).click();
  await page.getByRole('button', {name:'Prepare another import', exact:true}).waitFor();
  assert.equal((await receipt()).operation.id, movieReceipt.operation.id);
  assert.equal(await readFile(`${scratch}/movies/movie.mkv`, 'utf8'), 'scratch movie media');
  await page.reload();
  await page.getByRole('button', {name:'Movies', exact:true}).click();
  await page.getByRole('button', {name:/Fixture movie.*1 files/}).click();
  await page.getByRole('button', {name:'Monitor movie', exact:true}).waitFor();
  assert.match(await page.getByRole('article').innerText(), /1 associated files/);
  await page.getByRole('button', {name:'TV', exact:true}).click();
  await page.getByRole('button', {name:/Fixture series.*1 files/}).click();
  await page.getByRole('button', {name:'Monitor series', exact:true}).waitFor();
  await page.getByRole('button', {name:'Unmonitor Pilot', exact:true}).waitFor();
  await page.getByRole('button', {name:'Monitor Second episode', exact:true}).waitFor();
  assert.match(await page.getByRole('article').innerText(), /pilot.mkv/);
  await verifyProviders();
  await page.setViewportSize({width:390,height:844});
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), 'Mobile viewport must not overflow');
  if (process.env.UI_SCREENSHOT) await page.screenshot({path:process.env.UI_SCREENSHOT, fullPage:true});
  assert.deepEqual(errors, []);
  console.log('PASS: both-domain add/monitor/import, typed ID collision, reload, stale lookup, lost-response reconciliation, provider create/edit/test/credentials/revision conflict/reload, mobile overflow and browser errors');
} finally { await browser.close(); }
