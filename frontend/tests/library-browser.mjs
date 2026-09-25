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
  await page.setViewportSize({width:390,height:844});
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), 'Mobile viewport must not overflow');
  if (process.env.UI_SCREENSHOT) await page.screenshot({path:process.env.UI_SCREENSHOT, fullPage:true});
  assert.deepEqual(errors, []);
  console.log('PASS: both-domain add/monitor/import, typed ID collision, reload, stale lookup, lost-response reconciliation, mobile overflow and browser errors');
} finally { await browser.close(); }
