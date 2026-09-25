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
// This Playwright version treats an async waitForFunction predicate as a truthy
// Promise even when it resolves false. Await native reads explicitly and bound time.
async function waitForCommand(route, id, status, recordsRemoved) {
  const deadline=Date.now()+10000;
  let last;
  while(Date.now()<deadline){
    const response=await page.request.get(`${origin}${route}/${id}`,{timeout:Math.max(1,deadline-Date.now())});
    assert.equal(response.status(),200);
    last=await response.json();assert.equal(last.id,id);
    if(last.status===status&&(recordsRemoved===undefined||last.records_removed===recordsRemoved))return last;
    await new Promise(resolve=>setTimeout(resolve,100));
  }
  assert.fail(`Command ${id} did not reach ${status}: ${JSON.stringify(last)}`);
}
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

async function verifyReleaseSearch() {
  const providers=(await (await page.request.get(`${origin}/api/v1/providers`)).json()).items;
  let indexer=providers.find(p=>p.name==='Browser torznab');
  const enabled=await page.request.put(`${origin}/api/v1/providers/${indexer.id}`,{data:{revision:indexer.revision,name:indexer.name,enabled:true,priority:indexer.priority,settings:indexer.settings}});
  assert.equal(enabled.status(),200);indexer=await enabled.json();
  for(const domain of ['tv','movies']){
    const profileResponse=await page.request.post(`${origin}/api/v1/${domain}/quality-profiles`,{data:{name:`Browser ${domain} WEB`,items:[{kind:'quality',quality_id:3,allowed:true}],policy:{upgrade_allowed:true,cutoff:{kind:'quality',quality_id:3},min_format_score:0,cutoff_format_score:0,min_upgrade_format_score:1,language_id:domain==='movies'?-2:null,format_items:[]}}});
    assert.equal(profileResponse.status(),201,await profileResponse.text());const profile=await profileResponse.json();
    await page.getByRole('button',{name:'Library',exact:true}).click();
    await page.getByRole('button',{name:domain==='tv'?'TV':'Movies',exact:true}).click();
    await page.getByRole('button',{name:new RegExp(`Fixture ${domain==='tv'?'series':'movie'}.*1 files`)}).click();
    await page.getByText('Library settings',{exact:true}).click();
    await page.getByLabel('Quality profile',{exact:true}).selectOption(String(profile.id));
    if(domain==='tv'){await page.getByLabel('Series type',{exact:true}).selectOption('standard');await page.getByLabel('Scene numbering',{exact:true}).selectOption('false');}
    else await page.getByLabel('Minimum availability',{exact:true}).selectOption('released');
    const saved=page.waitForResponse(r=>r.request().method()==='PUT'&&r.url().includes(domain==='tv'?'/api/v1/tv/series/':'/api/v1/movies/'));
    await page.getByRole('button',{name:'Save settings',exact:true}).click();
    assert.equal((await saved).status(),200);
    const collection=domain==='tv'?'/api/v1/tv/series':'/api/v1/movies';
    const item=(await (await page.request.get(`${origin}${collection}`)).json()).items.find(i=>i.title.startsWith('Fixture'));
    assert.equal(item.settings.quality_profile_id,profile.id,'Profile selection must persist through the real library writer');
    await page.reload();await page.getByRole('button',{name:domain==='tv'?'TV':'Movies',exact:true}).click();
    await page.getByRole('button',{name:new RegExp(`Fixture ${domain==='tv'?'series':'movie'}.*1 files`)}).click();
    await page.getByText('Library settings',{exact:true}).click();assert.equal(await page.getByLabel('Quality profile',{exact:true}).inputValue(),String(profile.id));
    await page.getByRole('button',{name:domain==='tv'?'Search releases for Second episode':'Search movie releases',exact:true}).click();
    const panel=page.getByRole('region',{name:'Release search'});
    await panel.getByLabel('Search indexer').selectOption(indexer.id);
    const response=page.waitForResponse(r=>r.url().endsWith('/api/v1/release-search'));
    await panel.getByRole('button',{name:'Find releases',exact:true}).click();
    const releases=await response;assert.equal(releases.status(),200);const result=await releases.json();assert.equal(result.items.length,1);
    assert.equal(result.items[0].decision.target.media_type,domain);
    await panel.getByText('1 releases on this page.',{exact:true}).waitFor();
    await panel.getByText(`Background release delays · ${domain==='tv'?'TV':'Movies'}`,{exact:true}).click();
    await panel.getByLabel('Torrent delay (minutes)').fill('60');await panel.getByLabel('Usenet delay (minutes)').fill('30');
    await panel.getByRole('button',{name:'Save release delays',exact:true}).click();await panel.getByText('Release delay settings saved.',{exact:true}).waitFor();
    assert.deepEqual(await (await page.request.get(`${origin}/api/v1/release-policies/${domain}`)).json(),{torrent_delay_minutes:60,usenet_delay_minutes:30,availability_delay_days:0});
  }
}

async function verifyRss() {
  const providers=(await (await page.request.get(`${origin}/api/v1/providers`)).json()).items;
  const indexer=providers.find(p=>p.name==='Browser torznab'),client=providers.find(p=>p.name==='Browser qbittorrent');
  await page.getByRole('button',{name:'RSS',exact:true}).click();
  const panel=page.getByRole('region',{name:'RSS automation'});
  for(const domain of ['tv','movies']){
    await panel.getByLabel('RSS media').selectOption(domain);
    await panel.getByLabel('RSS indexer').selectOption(indexer.id);await panel.getByLabel('RSS download client').selectOption(client.id);
    await panel.getByRole('button',{name:'Save RSS schedule',exact:true}).click();await panel.getByText('RSS schedule saved.',{exact:true}).waitFor();
    const schedules=await (await page.request.get(`${origin}/api/v1/rss/schedules`)).json();
    assert.ok(schedules.some(s=>s.target.media_type===domain&&!s.enabled&&s.interval_seconds===900));
    const created=page.waitForResponse(r=>r.url().endsWith('/api/v1/rss/commands')&&r.request().method()==='POST');
    await panel.getByRole('button',{name:'Run RSS now',exact:true}).click();const response=await created;assert.equal(response.status(),202);const command=await response.json();
    assert.equal(command.target.media_type,domain);await waitForCommand('/api/v1/rss/commands',command.id,'succeeded');
    await panel.getByRole('button',{name:'Check RSS status',exact:true}).click();
    await panel.getByText('1 receipts for '+domain+'.',{exact:true}).waitFor();
    const candidates=await (await page.request.get(`${origin}/api/v1/rss/candidates?media_type=${domain}`)).json();
    assert.equal(candidates.items[0].source.media_type,domain);assert.equal(candidates.items[0].status,'rejected');
  }
  let posts=0;
  await page.route('**/api/v1/rss/commands',async route=>{if(route.request().method()!=='POST')return route.continue();posts++;const response=await route.fetch();assert.equal(response.status(),202);await route.abort('failed');});
  await panel.getByRole('button',{name:'Run RSS now',exact:true}).click();
  await panel.getByRole('alert').filter({hasText:'request may have committed'}).waitFor();
  assert.equal(await panel.getByRole('button',{name:'Run RSS now',exact:true}).isDisabled(),true);assert.equal(posts,1);
  await page.unroute('**/api/v1/rss/commands');await panel.getByRole('button',{name:'Check RSS status',exact:true}).click();
  await panel.getByText('RSS status checked. Inspect recorded commands and receipts before another action.',{exact:true}).waitFor();
}

async function verifyActivity() {
  const providerOrigin=process.env.UI_PROVIDER_ORIGIN;
  // Reuse the provider created through this browser, enabling its durable read worker.
  await page.getByLabel('Enabled',{exact:true}).check();
  await page.getByRole('button',{name:'Save provider',exact:true}).click();
  await page.getByRole('button',{name:'Test saved provider',exact:true}).waitFor();
  await page.waitForFunction(()=>!Array.from(document.querySelectorAll('button')).find(b=>b.textContent==='Test saved provider')?.disabled);
  const providers=await (await page.request.get(`${origin}/api/v1/providers`)).json();
  const provider=providers.items.find(p=>p.name==='Browser qbittorrent');
  assert.equal(provider.enabled,true);
  const mode=async(value)=>assert.equal((await page.request.post(`${providerOrigin}/fixture-mode?mode=${value}`)).status(),204);
  const commands=async()=> (await (await page.request.get(`${origin}/api/v1/commands`)).json()).items;
  const waitCommand=async(id,status)=>{
    for(let i=0;i<120;i++) {
      const result=await (await page.request.get(`${origin}/api/v1/commands/${id}`)).json();
      if(result.status===status)return result;
      await page.waitForTimeout(250);
    }
    assert.fail(`Command ${id} did not reach ${status}`);
  };
  const trigger=async()=>{
    const response=page.waitForResponse(r=>r.url().endsWith('/api/v1/commands')&&r.request().method()==='POST');
    await page.getByRole('button',{name:'Refresh downloads',exact:true}).click();
    const result=await response;assert.equal(result.status(),202);
    return result.json();
  };
  await page.getByRole('button',{name:'Activity',exact:true}).click();
  await page.getByLabel('Download client').selectOption(provider.id);
  await page.getByText('No successful snapshot yet. Download contents are unknown.',{exact:true}).waitFor();
  for(const media of ['tv','movies']) {
    await page.getByLabel('Refresh media').selectOption(media);
    const command=await trigger();
    assert.equal(command.target.media_type,media);
    const completed=await waitCommand(command.id,'succeeded');assert.equal(completed.attempts,1);
    const queue=await (await page.request.get(`${origin}/api/v1/queue?provider_id=${provider.id}&media_type=${media}`)).json();
    assert.equal(queue.items.length,1);assert.equal(queue.items[0].association,null);
    assert.equal(queue.items[0].download.category,media);
  }
  await page.getByRole('button',{name:'Reload activity',exact:true}).click();
  await page.getByRole('cell',{name:/Fixture download/}).waitFor();
  await mode(1);
  const failing=await trigger();
  await waitCommand(failing.id,'retry_wait');
  await page.getByRole('button',{name:'Reload activity',exact:true}).click();
  await page.getByRole('heading',{name:'refresh_downloads: retry_wait',exact:true}).waitFor();
  const failed=await waitCommand(failing.id,'failed');assert.equal(failed.attempts,3);
  await page.getByRole('button',{name:'Reload activity',exact:true}).click();
  await page.getByText(failed.error_code,{exact:false}).first().waitFor();
  assert.doesNotMatch(await page.locator('body').innerText(),/PRIVATE_FIXTURE/);
  assert.ok(await page.getByRole('cell',{name:/Fixture download/}).isVisible(),'Failed read must retain dated successful observation');
  await mode(2);
  const cancelling=await trigger();await waitCommand(cancelling.id,'running');
  await page.getByRole('button',{name:'Cancel command',exact:true}).click();
  await waitCommand(cancelling.id,'cancelled');
  await mode(0);
  await page.getByRole('button',{name:'Delete command history',exact:true}).click();
  await page.getByRole('button',{name:'Confirm delete history',exact:true}).click();
  await page.getByText('Terminal command history deleted. Downloads and media were not deleted.',{exact:true}).waitFor();
  assert.equal((await page.request.get(`${origin}/api/v1/commands/${cancelling.id}`)).status(),404);
  let posted=0, lostId;
  await page.route('**/api/v1/commands',async route=>{
    if(route.request().method()!=='POST')return route.continue();
    posted++;
    const response=await route.fetch();lostId=(await response.json()).id;
    await route.abort('failed');
  });
  await page.getByRole('button',{name:'Refresh downloads',exact:true}).click();
  await page.getByRole('alert').filter({hasText:/may have committed/}).waitFor();
  assert.equal(await page.getByRole('button',{name:'Refresh downloads',exact:true}).isDisabled(),true);
  await page.unroute('**/api/v1/commands');
  await waitCommand(lostId,'succeeded');
  await page.getByRole('button',{name:'Reload activity',exact:true}).click();
  await page.getByRole('button',{name:`Inspect command ${lostId}`,exact:true}).waitFor();
  assert.equal(posted,1,'Unknown request outcome must not automatically replay');
  // Explicit schedule creation is the only point that enables repeated refreshes.
  await page.getByLabel('Interval seconds',{exact:true}).fill('86400');
  await page.getByLabel('Enable schedule',{exact:true}).check();
  await page.getByRole('button',{name:'Save refresh schedule',exact:true}).click();
  await page.getByText('Refresh schedule saved.',{exact:true}).waitFor();
  let schedules=await (await page.request.get(`${origin}/api/v1/download-refresh/schedules`)).json();
  assert.equal(schedules.length,1);assert.equal(schedules[0].target.media_type,'movies');
  assert.equal(schedules[0].enabled,true);assert.equal(schedules[0].interval_seconds,86400);
  const before=await commands();
  await page.reload();await page.getByRole('button',{name:'Activity',exact:true}).click();
  await page.getByLabel('Download client').selectOption(provider.id);
  await page.getByLabel('Refresh media').selectOption('movies');
  await page.waitForFunction(()=>Array.from(document.querySelectorAll('input[type=number]')).some(input=>input.value==='86400'));
  assert.equal(await page.getByLabel('Interval seconds',{exact:true}).inputValue(),'86400');
  assert.equal(await page.getByLabel('Enable schedule',{exact:true}).isChecked(),true);
  await page.getByLabel('Enable schedule',{exact:true}).uncheck();
  await page.getByRole('button',{name:'Save refresh schedule',exact:true}).click();
  await page.getByText('Refresh schedule saved.',{exact:true}).waitFor();
  schedules=await (await page.request.get(`${origin}/api/v1/download-refresh/schedules`)).json();
  assert.equal(schedules[0].enabled,false);assert.equal(schedules[0].revision,2);
  await page.getByRole('button',{name:'Delete schedule',exact:true}).click();
  await page.getByRole('button',{name:'Confirm delete schedule',exact:true}).click();
  await page.getByText('Refresh schedule deleted.',{exact:true}).waitFor();
  assert.deepEqual(await (await page.request.get(`${origin}/api/v1/download-refresh/schedules`)).json(),[]);
  // Reads/reload must not enqueue; only the immediately due saved schedule may add one command.
  assert.ok((await commands()).length<=before.length+1);
  const filtered=page.waitForResponse(r=>r.url().includes('/api/v1/commands?')&&r.url().includes('media_type=tv'));
  await page.getByLabel('Command media').selectOption('tv');
  assert.ok((await (await filtered).json()).items.every(command=>command.target.media_type==='tv'));
}

async function verifyMetadataRefresh() {
  const metadataOrigin=process.env.UI_METADATA_ORIGIN;
  assert.ok(metadataOrigin,'Set UI_METADATA_ORIGIN from the owned fixture');
  assert.equal(new URL(metadataOrigin).hostname,'127.0.0.1');
  const mode=async(value)=>assert.equal((await page.request.post(`${metadataOrigin}/fixture-metadata-mode?mode=${value}`)).status(),204);
  const history=async(domain)=>{const response=await page.request.get(`${origin}/api/v1/metadata-refresh/commands?media_type=${domain}&${domain==='tv'?'series_id':'movie_id'}=1`);assert.equal(response.status(),200);return (await response.json()).items;};
  const panel=page.getByRole('region',{name:'Metadata refresh',exact:true});
  await page.getByRole('button',{name:'Library',exact:true}).click();
  await page.getByRole('button',{name:'TV',exact:true}).click();
  await page.getByRole('button',{name:/Fixture series.*1 files/}).click();
  await panel.getByRole('button',{name:'Refresh metadata',exact:true}).waitFor();
  await page.getByText('Library settings',{exact:true}).click();
  await page.getByLabel('Series type').selectOption('daily');
  await mode(1);
  // The server accepts a command but its response is lost. UI must not retry the POST.
  let postCount=0;
  await page.route('**/api/v1/metadata-refresh/commands',async route=>{
    if(route.request().method()!=='POST'){await route.continue();return;}
    postCount++;await route.fetch();await route.abort('failed');
  });
  await panel.getByRole('button',{name:'Refresh metadata',exact:true}).click();
  await panel.getByText(/request may have committed/).waitFor();
  assert.equal(await panel.getByRole('button',{name:'Refresh metadata',exact:true}).isDisabled(),true);
  await page.unroute('**/api/v1/metadata-refresh/commands');
  await panel.getByRole('button',{name:'Check metadata status',exact:true}).click();
  await panel.getByRole('heading',{name:'refresh_series: succeeded',exact:true}).waitFor();
  assert.equal(postCount,1);assert.equal((await history('tv')).length,1);
  assert.equal(await page.getByLabel('Series type').inputValue(),'daily','Completion must preserve unsaved form edits');
  await panel.getByRole('button',{name:'Reload library details',exact:true}).click();
  await page.getByRole('article').getByRole('heading',{name:'Refreshed series',exact:true}).waitFor();
  await page.getByRole('button',{name:'Unmonitor Refreshed pilot',exact:true}).waitFor();
  await page.getByRole('button',{name:'Monitor New episode',exact:true}).waitFor();
  assert.match(await page.getByRole('article').innerText(),/pilot.mkv/);
  await page.getByRole('button',{name:'Monitor series',exact:true}).waitFor();
  await page.getByRole('button',{name:'Movies',exact:true}).click();
  await page.getByRole('button',{name:/Fixture movie.*1 files/}).click();
  await panel.getByText('0 metadata commands for this title.',{exact:true}).waitFor();
  await panel.getByRole('button',{name:'Refresh metadata',exact:true}).click();
  await panel.getByRole('heading',{name:'refresh_movie: succeeded',exact:true}).waitFor();
  const movieHistory=await history('movies');assert.equal(movieHistory.length,1);assert.deepEqual(movieHistory[0].target,{media_type:'movies',movie_id:1});
  await panel.getByRole('button',{name:'Reload library details',exact:true}).click();
  await page.getByRole('article').getByRole('heading',{name:'Refreshed movie',exact:true}).waitFor();
  await page.getByRole('button',{name:'Monitor movie',exact:true}).waitFor();
  assert.match(await page.getByRole('article').innerText(),/1 associated files/);
  await mode(3);
  await panel.getByRole('button',{name:'Refresh metadata',exact:true}).click();
  await panel.getByRole('button',{name:'Cancel metadata refresh',exact:true}).click();
  await panel.getByRole('heading',{name:'refresh_movie: cancelled',exact:true}).waitFor();
  await panel.getByRole('button',{name:'Delete metadata command history',exact:true}).click();
  await panel.getByRole('button',{name:'Confirm delete metadata history',exact:true}).click();
  assert.equal((await history('movies')).some(command=>command.status==='cancelled'),false);
  await mode(2);
  await panel.getByRole('button',{name:'Refresh metadata',exact:true}).click();
  await panel.getByRole('heading',{name:'refresh_movie: retry_wait',exact:true}).waitFor();
  assert.match(await panel.innerText(),/metadata_unavailable/);
  await panel.getByRole('button',{name:'Cancel metadata refresh',exact:true}).click();
  await panel.getByRole('heading',{name:'refresh_movie: cancelled',exact:true}).waitFor();
  // Leave while TV refresh is running; late reads must never replace movie detail.
  await mode(3);
  await page.getByRole('button',{name:'TV',exact:true}).click();
  await page.getByRole('button',{name:/Refreshed series.*1 files/}).click();
  // Bind this assertion to the newly accepted command, never an earlier success.
  const accepted=page.waitForResponse(response=>response.url().endsWith('/api/v1/metadata-refresh/commands')&&response.request().method()==='POST');
  await panel.getByRole('button',{name:'Refresh metadata',exact:true}).click();
  const acceptedResponse=await accepted;assert.equal(acceptedResponse.status(),202);
  const lateCommand=await acceptedResponse.json();
  assert.deepEqual(lateCommand.target,{media_type:'tv',series_id:1});
  await page.getByRole('button',{name:'Movies',exact:true}).click();
  await page.getByRole('button',{name:/Refreshed movie.*1 files/}).click();
  await waitForCommand('/api/v1/metadata-refresh/commands',lateCommand.id,'succeeded');
  await page.getByRole('article').getByRole('heading',{name:'Refreshed movie',exact:true}).waitFor();
  assert.ok((await history('movies')).every(command=>command.target.media_type==='movies'));
  await mode(1);
  const before=(await history('movies')).length;
  await page.reload();await page.getByRole('button',{name:'Movies',exact:true}).click();
  await page.getByRole('button',{name:/Refreshed movie.*1 files/}).click();
  await panel.getByRole('button',{name:'Check metadata status',exact:true}).click();
  assert.equal((await history('movies')).length,before,'Readback and reload do not enqueue metadata refresh');
  assert.equal(await readFile(`${scratch}/tv/pilot.mkv`,'utf8'),'scratch episode media');
  assert.equal(await readFile(`${scratch}/movies/movie.mkv`,'utf8'),'scratch movie media');
}

async function verifyBlocklist() {
  const imported=await page.request.post(`${origin}/api/fixture/import-blocklists`);
  assert.equal(imported.status(),200,await imported.text());
  const reports=await imported.json();assert.equal(reports.length,2);
  for(const report of reports){assert.equal(report.applied,true);assert.equal(report.conflicts,0);}
  const read=async(query='')=>{const response=await page.request.get(`${origin}/api/v1/blocklist?limit=100${query}`);assert.equal(response.status(),200);return response.json();};
  let records=await read();assert.equal(records.total,27);
  const tv=records.items.find(item=>item.source_title==='Blocked TV pack');
  const movie=records.items.find(item=>item.source_title==='Blocked movie');
  assert.equal(tv.id.source_id,movie.id.source_id);
  assert.equal(tv.target.series_id,movie.target.movie_id,'Equal numeric parent IDs must retain distinct domains');
  assert.equal(tv.target.episode_ids.length,2);assert.equal(new Set(tv.target.episode_ids).size,2);
  assert.notEqual(tv.id.fingerprint,movie.id.fingerprint);
  assert.ok(!JSON.stringify(records).includes('PRIVATE_BLOCKLIST_SENTINEL'));
  assert.ok(!JSON.stringify(records).includes('private-indexer'));
  await page.getByRole('button',{name:'Blocklist',exact:true}).click();
  await page.getByText('27 matching entries. Selection applies only to the current page.',{exact:true}).waitFor();
  const next=page.waitForResponse(response=>response.url().includes('/api/v1/blocklist?')&&response.url().includes('offset=25'));
  await page.getByRole('button',{name:'Next blocklist page',exact:true}).click();
  assert.equal((await (await next).json()).items.length,2);
  await page.getByLabel('Blocklist media').selectOption('tv');
  await page.getByLabel('Library IDs',{exact:true}).fill(`${tv.target.series_id}, 999999`);
  await page.getByLabel('Blocklist protocol').selectOption('torrent');
  const filtered=page.waitForResponse(response=>response.url().includes('/api/v1/blocklist?')&&response.url().includes('series_ids='));
  await page.getByRole('button',{name:'Apply blocklist filters',exact:true}).click();
  const filterResponse=await filtered;
  assert.equal(new URL(filterResponse.url()).searchParams.get('series_ids'),`${tv.target.series_id},999999`,'Validated CSV whitespace is canonicalized before sending');
  assert.equal((await filterResponse.json()).total,1);
  await page.getByRole('button',{name:'Blocked TV pack',exact:true}).click();
  const detail=page.getByRole('region',{name:'Blocklist entry detail'});
  assert.match(await detail.innerText(),new RegExp(`episodes ${tv.target.episode_ids.join(', ')}`));
  assert.match(await detail.innerText(),/source_snapshot/);
  assert.ok(!(await page.locator('body').innerText()).includes('PRIVATE_BLOCKLIST_SENTINEL'));
  // Single removal requires confirmation; abandoning it has no effect.
  await page.getByRole('button',{name:'Remove this entry',exact:true}).click();
  await page.getByRole('button',{name:'Keep blocklist entries',exact:true}).click();
  assert.equal((await read()).total,27);
  await page.getByRole('button',{name:'Remove this entry',exact:true}).click();
  await page.getByRole('button',{name:'Confirm blocklist removal',exact:true}).click();
  await page.getByText('Blocklist entries removed. Library records, files and client downloads remain intact.',{exact:true}).waitFor();
  assert.equal((await read()).total,26);
  await page.getByLabel('Blocklist media').selectOption('');
  await page.getByLabel('Library IDs',{exact:true}).fill('');
  await page.getByLabel('Blocklist protocol').selectOption('');
  await page.getByRole('button',{name:'Apply blocklist filters',exact:true}).click();
  await page.getByLabel('Select movies Blocked movie (1)',{exact:true}).check();
  await page.getByLabel('Select tv Blocked TV pilot (2)',{exact:true}).check();
  await page.getByRole('button',{name:'Remove selected entries (2)',exact:true}).click();
  let deletes=0;
  await page.route('**/api/v1/blocklist/bulk',async route=>{deletes++;await route.fetch();await route.abort('failed');});
  await page.getByRole('button',{name:'Confirm blocklist removal',exact:true}).click();
  await page.getByText(/removal may have completed/).waitFor();
  assert.equal(await page.getByRole('button',{name:'Remove selected entries (2)',exact:true}).isDisabled(),true);
  await page.unroute('**/api/v1/blocklist/bulk');
  await page.getByRole('button',{name:'Refresh blocklist',exact:true}).click();
  await page.getByText('24 matching entries. Selection applies only to the current page.',{exact:true}).waitFor();
  assert.equal(deletes,1,'Lost deletion response must never cause automatic replay');
  const replay=await page.request.post(`${origin}/api/fixture/import-blocklists`);assert.equal(replay.status(),200);
  const replayReports=await replay.json();assert.equal(replayReports.length,2);
  for(const report of replayReports){assert.equal(report.applied,true);assert.equal(report.conflicts,0);}
  records=await read();assert.equal(records.total,24,'Same-snapshot replay must retain removals');
  assert.ok(records.items.every(item=>item.target.media_type==='tv'));
  await page.reload();await page.getByRole('button',{name:'Blocklist',exact:true}).click();
  await page.getByText('24 matching entries. Selection applies only to the current page.',{exact:true}).waitFor();
  assert.equal(await readFile(`${scratch}/tv/pilot.mkv`,'utf8'),'scratch episode media');
  assert.equal(await readFile(`${scratch}/movies/movie.mkv`,'utf8'),'scratch movie media');
}


async function verifyClearBlocklist() {
  const importPair=async(route)=>{const response=await page.request.post(`${origin}/api/fixture/${route}`);assert.equal(response.status(),200);const reports=await response.json();assert.equal(reports.length,2);for(const report of reports){assert.equal(report.applied,true);assert.equal(report.conflicts,0);}};
  const read=async()=> (await page.request.get(`${origin}/api/v1/blocklist?limit=100`)).json();
  await importPair('import-clear-blocklists');assert.equal((await read()).total,51);
  await page.getByLabel('Blocklist media').selectOption('movies');
  await page.getByRole('button',{name:'Apply blocklist filters',exact:true}).click();
  await page.getByText('1 matching entries. Selection applies only to the current page.',{exact:true}).waitFor();
  await page.getByLabel('Clear domain').selectOption('tv');
  await page.getByRole('button',{name:'Clear entire domain',exact:true}).click();
  assert.match(await page.getByRole('region',{name:'Confirm whole-domain clear'}).innerText(),/regardless of the current filters/);
  await page.getByRole('button',{name:'Keep domain blocklist',exact:true}).click();assert.equal((await read()).total,51);
  // Occupy the same worker with a bounded read so queued cancellation is observable.
  await page.request.post(`${process.env.UI_METADATA_ORIGIN}/fixture-metadata-mode?mode=3`);
  const hold=await (await page.request.post(`${origin}/api/v1/metadata-refresh/commands`,{data:{target:{media_type:'tv',series_id:1},priority:'normal'}})).json();
  await waitForCommand('/api/v1/metadata-refresh/commands',hold.id,'running');
  await page.getByLabel('Clear domain').selectOption('movies');
  await page.getByRole('button',{name:'Clear entire domain',exact:true}).click();
  const queuedResponse=page.waitForResponse(r=>r.url().endsWith('/api/v1/blocklist/clear-commands')&&r.request().method()==='POST');
  await page.getByRole('button',{name:'Confirm clear Movies blocklist',exact:true}).click();
  const queued=await (await queuedResponse).json();
  await page.getByRole('button',{name:'Cancel clear command',exact:true}).click();
  await waitForCommand('/api/v1/blocklist/clear-commands',queued.id,'cancelled');
  assert.equal((await read()).total,51);
  await page.getByLabel('Clear domain').selectOption('tv');
  await page.getByRole('button',{name:'Clear entire domain',exact:true}).click();
  let posts=0, accepted;
  await page.route('**/api/v1/blocklist/clear-commands',async route=>{if(route.request().method()!=='POST')return route.continue();posts++;const result=await route.fetch();accepted=await result.json();await route.abort();});
  await page.getByRole('button',{name:'Confirm clear TV blocklist',exact:true}).click();
  await page.getByText(/The request may have committed/).waitFor();
  assert.equal(await page.getByRole('button',{name:'Clear entire domain',exact:true}).isDisabled(),true);
  await page.unroute('**/api/v1/blocklist/clear-commands');
  await waitForCommand('/api/v1/blocklist/clear-commands',accepted.id,'succeeded');
  await page.getByRole('button',{name:'Check clear command status',exact:true}).click();
  await page.getByRole('heading',{name:'tv clear: succeeded',exact:true}).waitFor();
  assert.match(await page.getByRole('region',{name:'Clear command detail'}).innerText(),/Records removed\s+50/);assert.equal(posts,1);
  let records=await read();assert.equal(records.total,1);assert.equal(records.items[0].target.media_type,'movies');
  await importPair('import-blocklists');await importPair('import-clear-blocklists');assert.equal((await read()).total,1);
  await page.getByRole('button',{name:'Delete clear command history',exact:true}).click();
  await page.getByRole('button',{name:'Confirm delete clear history',exact:true}).click();
  await page.getByLabel('Clear domain').selectOption('movies');
  await page.getByRole('button',{name:'Clear entire domain',exact:true}).click();
  const movieResponse=page.waitForResponse(r=>r.url().endsWith('/api/v1/blocklist/clear-commands')&&r.request().method()==='POST');
  await page.getByRole('button',{name:'Confirm clear Movies blocklist',exact:true}).click();
  const movieClear=await (await movieResponse).json();
  await waitForCommand('/api/v1/blocklist/clear-commands',movieClear.id,'succeeded',1);
  await importPair('import-blocklists');await importPair('import-clear-blocklists');assert.equal((await read()).total,0);
  await page.reload();await page.getByRole('button',{name:'Blocklist',exact:true}).click();
  await page.getByText('0 matching entries. Selection applies only to the current page.',{exact:true}).waitFor();
  assert.equal(await readFile(`${scratch}/tv/pilot.mkv`,'utf8'),'scratch episode media');
  assert.equal(await readFile(`${scratch}/movies/movie.mkv`,'utf8'),'scratch movie media');
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
  await verifyActivity();
  await verifyReleaseSearch();
  await verifyRss();
  await page.setViewportSize({width:390,height:844});
  assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),'RSS receipts must wrap at mobile width');
  if(process.env.UI_SCREENSHOT)await page.screenshot({path:`${process.env.UI_SCREENSHOT}.rss.png`,fullPage:true});
  await page.setViewportSize({width:1280,height:720});
  await verifyMetadataRefresh();
  await verifyBlocklist();
  await verifyClearBlocklist();
  await page.setViewportSize({width:390,height:844});
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), 'Mobile viewport must not overflow');
  if (process.env.UI_SCREENSHOT) await page.screenshot({path:process.env.UI_SCREENSHOT, fullPage:true});
  assert.deepEqual(errors, []);
  console.log('PASS: both-domain add/monitor/import, typed ID collision, reload, stale lookup, lost-response reconciliation, provider create/edit/test/credentials/revision conflict/reload, Activity scoped refresh/retry/cancel/history/schedules, metadata typed refresh/preservation/uncertainty/cancellation/retry, imported blocklist filters/identity/privacy/single-bulk removal/replay, whole-domain clear confirmation/cancel/unknown-response/both-domain completion/replay, profile assignment/reload and release decisions/policies/RSS rejection/schedule/request loss in both domains, mobile overflow and browser errors');
} catch (error) {
  console.error('Browser errors:',errors);
  if(process.env.UI_SCREENSHOT)await page.screenshot({path:process.env.UI_SCREENSHOT,fullPage:true});
  throw error;
} finally { await browser.close(); }
