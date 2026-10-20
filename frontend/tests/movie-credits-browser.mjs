// Owned fixture: real add/read/refresh. Read-error, incomplete-page and delayed-response cases use overlays.
import assert from 'node:assert/strict';
import {realpath, readFile} from 'node:fs/promises';
import {basename, isAbsolute, join} from 'node:path';
import {pathToFileURL} from 'node:url';
const {chromium} = await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE || '/usr/lib/node_modules/playwright/index.mjs').href);
const origin = process.env.UI_URL, metadata = process.env.UI_METADATA_ORIGIN;
for (const value of [origin, metadata]) assert.equal(new URL(value).hostname, '127.0.0.1');
assert.ok(process.env.UI_SCRATCH && isAbsolute(process.env.UI_SCRATCH));
const scratch = await realpath(process.env.UI_SCRATCH);
assert.ok(basename(scratch).startsWith('hrrdarr-library-ui-'));
const browser = await chromium.launch({headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? {executablePath: process.env.PLAYWRIGHT_EXECUTABLE} : {})});
const page = await browser.newPage();
page.setDefaultTimeout(20000);
const errors = [], writes = [], external = [], blockedImages = [];
page.on('pageerror', error => errors.push(error.message));
page.on('dialog', dialog => {errors.push(`Unexpected dialog: ${dialog.message()}`); void dialog.dismiss();});
page.on('request', request => {if (!['GET', 'HEAD'].includes(request.method())) writes.push([request.method(), new URL(request.url()).pathname]);});
// Block the known fixture image before network to exercise image-error fallback without contacting TMDb.
await page.route('**/*', route => {
  const url = new URL(route.request().url());
  if (['http:', 'https:'].includes(url.protocol) && url.hostname !== '127.0.0.1') {
    if (url.href === 'https://image.tmdb.org/t/p/original/owned-credits-missing.jpg' && route.request().resourceType() === 'image') blockedImages.push(url.href);
    else external.push(url.href);
    return route.abort();
  }
  return route.continue();
});
const panel = page.getByRole('region', {name: 'Movie credits', exact: true});
const refresh = page.getByRole('region', {name: 'Metadata refresh', exact: true});
const domain = page.getByRole('navigation', {name: 'Library type'});
async function get(path) {
  const response = await page.request.get(origin + path, {timeout: 10000});
  assert.equal(response.status(), 200, await response.text()); return response.json();
}
async function until(read, accept, label) {
  const deadline = Date.now() + 30000; let last;
  while (Date.now() < deadline) {last = await read(); if (accept(last)) return last; await page.waitForTimeout(100);}
  assert.fail(`${label}: ${JSON.stringify(last)}`);
}
async function ready(button) {await button.waitFor(); await until(() => button.isEnabled(), Boolean, 'Control remained disabled');}
async function mode(value) {assert.equal((await page.request.post(`${metadata}/fixture-metadata-mode?mode=${value}`, {timeout: 10000})).status(), 204);}
const credits = id => get(`/api/v1/movies/credits?movie_id=${id}&limit=500`);
async function add(media, search, title, directory) {
  await domain.getByRole('button', {name: media === 'tv' ? 'TV' : 'Movies', exact: true}).click();
  await page.getByRole('button', {name: media === 'tv' ? 'Add series' : 'Add movie', exact: true}).click();
  await page.getByLabel('Search catalogue', {exact: true}).fill(search);
  await page.getByRole('button', {name: 'Search', exact: true}).click();
  await page.getByRole('button', {name: new RegExp(`^${title} `)}).click();
  await page.getByLabel('Existing library directory', {exact: true}).fill(directory);
  await page.getByRole('button', {name: 'Add to library', exact: true}).click();
  await page.getByRole('article', {name: 'Library detail'}).getByRole('heading', {name: title, exact: true}).waitFor();
  const result = (await get(media === 'tv' ? '/api/v1/tv/series' : '/api/v1/movies')).items.find(item => item.title === title);
  assert.ok(result); return result;
}
async function choose(media, title) {
  await domain.getByRole('button', {name: media === 'tv' ? 'TV' : 'Movies', exact: true}).click();
  await page.getByRole('complementary', {name: 'Library collection'}).getByRole('button', {name: new RegExp(`^${title}`)}).click();
  await page.getByRole('article', {name: 'Library detail'}).getByRole('heading', {name: title, exact: true}).waitFor();
}
async function reloadCredits() {const button = panel.getByRole('button', {name: 'Reload movie credits', exact: true}); await ready(button); await button.click(); await until(() => panel.getAttribute('aria-busy'), value => value === 'false', 'Credits reload did not settle');}
async function cardCount(count) {await until(() => panel.locator('li[data-credit-id]').count(), value => value === count, 'Wrong displayed credit count');}
async function actualRefresh(value, status = 'succeeded') {
  await mode(value);
  const button = refresh.getByRole('button', {name: 'Refresh metadata', exact: true}); await ready(button);
  const accepted = page.waitForResponse(response => response.request().method() === 'POST' && new URL(response.url()).pathname === '/api/v1/metadata-refresh/commands');
  await button.click(); const response = await accepted; assert.equal(response.status(), 202, await response.text());
  const command = await response.json();
  const settled = await until(() => get(`/api/v1/metadata-refresh/commands/${command.id}`), value => ['succeeded', 'failed', 'cancelled'].includes(value.status), 'Metadata command did not settle');
  assert.equal(settled.status, status, JSON.stringify(settled));
  const check = refresh.getByRole('button', {name: 'Check metadata status', exact: true}); await ready(check); await check.click(); await ready(check);
  if (status === 'succeeded') {
    const before = writes.length;
    const reload = refresh.getByRole('button', {name: 'Reload library details', exact: true}); await ready(reload); await reload.click();
    await ready(panel.getByRole('button', {name: 'Reload movie credits', exact: true}));
    assert.equal(writes.length, before, 'Reload library details must be read-only');
  }
  return settled;
}
const librarySettings = value => Object.fromEntries(['id','tmdb_id','path','monitored','quality_profile_id','minimum_availability','tags'].map(key => [key, value[key]]));
try {
  await mode(6); await page.goto(origin);
  const first = await add('movies', 'Fixture', 'Fixture movie', join(scratch, 'movies'));
  const initial = await credits(first.id); assert.equal(initial.total, 105); assert.equal(initial.items.length, 105);
  assert.equal(initial.items.filter(row => row.type === 'cast').length, 102); assert.equal(initial.items.filter(row => row.type === 'crew').length, 3);
  const originalId = initial.items.find(row => row.credit_tmdb_id === 'cast-0').id;
  const second = await add('movies', 'Search 202', 'Search movie 202', join(scratch, 'search-movies-202'));
  await panel.getByText('No credits available for this movie.', {exact: true}).waitFor();
  await add('tv', 'Fixture', 'Fixture series', join(scratch, 'tv'));
  assert.equal(await panel.count(), 0);
  const beforeViewing = writes.length;
  const settings = librarySettings(await get(`/api/v1/movies/${first.id}`));
  const file = await readFile(join(scratch, 'incoming/movie.mkv'));
  await choose('movies', 'Fixture movie'); await cardCount(105);
  assert.equal(await panel.getByRole('region', {name: 'Cast', exact: true}).locator('li').count(), 102);
  assert.equal(await panel.getByRole('region', {name: 'Crew', exact: true}).locator('li').count(), 3);
  const namesakeCount = await panel.getByRole('link', {name: 'Same Name (TMDb)', exact: true}).count();
  if (namesakeCount !== 4) {
    console.log('Credit name diagnostics', JSON.stringify(await panel.getByRole('link').evaluateAll(links => links.filter((_, index) => index < 3 || index >= links.length - 3).map(link => ({text: link.textContent, inner: link.innerText, href: link.href, ariaLabel: link.getAttribute('aria-label')})))));
    console.log('First credit accessibility snapshot', await panel.locator('li[data-credit-id]').first().ariaSnapshot());
  }
  assert.equal(namesakeCount, 4);
  await panel.getByText('Character 101', {exact: true}).waitFor();
  await panel.getByText('Sound editor', {exact: true}).waitFor();
  await panel.getByRole('link', {name: '<img src=x onerror=alert(1)> (TMDb)', exact: true}).waitFor();
  assert.equal(await panel.locator('script,img[onerror]').count(), 0);
  const firstCard = panel.locator(`li[data-credit-id="${originalId}"]`);
  await firstCard.scrollIntoViewIfNeeded(); await firstCard.getByText('No headshot', {exact: true}).waitFor();
  const links = await panel.getByRole('link').evaluateAll(items => items.map(item => ({href: item.href, target: item.target, rel: item.rel})));
  assert.equal(links.length, 105);
  for (const link of links) {assert.match(link.href, /^https:\/\/www\.themoviedb\.org\/person\/[1-9][0-9]*$/); if (link.target === '_blank') assert.match(link.rel, /noopener/);}
  await panel.getByRole('heading', {name: 'Cast and crew', exact: true}).scrollIntoViewIfNeeded();
  await page.screenshot({path: '.do-not-commit/loop/iteration100-credits-desktop.png'});
  await page.setViewportSize({width: 390, height: 844});
  assert.equal(await panel.evaluate(node => node.scrollWidth <= node.clientWidth + 1), true, 'Credits panel overflows mobile width');
  await panel.getByRole('heading', {name: 'Cast and crew', exact: true}).scrollIntoViewIfNeeded();
  await page.screenshot({path: '.do-not-commit/loop/iteration100-credits-mobile.png'});
  await page.setViewportSize({width: 1280, height: 900});
  await reloadCredits(); await cardCount(105);
  // Explicit read-error overlay, followed by the actual backend response on retry.
  const creditUrl = url => url.pathname === '/api/v1/movies/credits' && url.searchParams.get('movie_id') === String(first.id);
  await page.route(creditUrl, route => route.fulfill({status: 503, contentType: 'application/json', body: JSON.stringify({error: {code: 'fixture_read_failure', message: 'Credits temporarily unavailable'}})}));
  await reloadCredits(); await panel.getByRole('alert').filter({hasText: 'Credits temporarily unavailable'}).waitFor();
  await page.unroute(creditUrl);
  await panel.getByRole('button', {name: 'Retry movie credits', exact: true}).click(); await cardCount(105);
  // Synthetic malformed pages derived from actual rows must never display a partial cast.
  for (const incomplete of [{...initial, offset: 1}, {...initial, items: initial.items.slice(0, 100)}]) {
    await page.route(creditUrl, route => route.fulfill({status: 200, contentType: 'application/json', body: JSON.stringify(incomplete)}));
    await reloadCredits();
    await panel.getByRole('alert').filter({hasText: 'The complete movie credits could not be loaded.'}).waitFor();
    assert.equal(await panel.locator('li[data-credit-id]').count(), 0, 'An incomplete page must not render partial credits');
    await page.unroute(creditUrl);
    await panel.getByRole('button', {name: 'Retry movie credits', exact: true}).click(); await cardCount(105);
  }
  // Delay an actual response; switching to another movie or TV must discard it.
  for (const [media, title] of [['movies', 'Search movie 202'], ['tv', 'Fixture series']]) {
    await choose('movies', 'Fixture movie'); await cardCount(105);
    let captured; const held = new Promise(resolve => {captured = resolve;});
    await page.route(creditUrl, async route => {const response = await route.fetch(); captured({route, response});});
    await panel.getByRole('button', {name: 'Reload movie credits', exact: true}).click();
    let timer;
    const old = await Promise.race([held, new Promise((_, reject) => {timer = setTimeout(() => reject(new Error('No delayed credits request')), 20000);})]).finally(() => clearTimeout(timer));
    await panel.getByText('Loading movie credits…', {exact: true}).waitFor();
    await choose(media, title);
    if (media === 'movies') await panel.getByText('No credits available for this movie.', {exact: true}).waitFor();
    else assert.equal(await panel.count(), 0);
    const delivered = page.waitForResponse(response => creditUrl(new URL(response.url())));
    await old.route.fulfill({response: old.response}); await (await delivered).finished(); await page.unroute(creditUrl);
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    assert.equal((await credits(second.id)).total, 0);
    assert.equal(await page.getByText('Character 101', {exact: true}).count(), 0);
    if (media === 'tv') assert.equal(await panel.count(), 0);
  }
  assert.equal(writes.length, beforeViewing, 'Viewing, navigation, credit reload and retry must not write');
  await choose('movies', 'Fixture movie'); await cardCount(105);
  await actualRefresh(7); await cardCount(1);
  await panel.getByRole('link', {name: 'Replacement name (TMDb)', exact: true}).waitFor(); await panel.getByText('Replacement role', {exact: true}).waitFor();
  const replacement = await credits(first.id); assert.equal(replacement.items[0].id, originalId);
  await actualRefresh(9); assert.deepEqual(await credits(first.id), replacement); await cardCount(1);
  await actualRefresh(10, 'failed'); assert.deepEqual(await credits(first.id), replacement);
  assert.equal((await get(`/api/v1/movies/${first.id}`)).title, 'Fixture movie', 'Malformed credits must reject the entire refresh');
  await actualRefresh(8); await panel.getByText('No credits available for this movie.', {exact: true}).waitFor(); assert.equal((await credits(first.id)).total, 0);
  assert.deepEqual(librarySettings(await get(`/api/v1/movies/${first.id}`)), settings);
  assert.deepEqual(await readFile(join(scratch, 'incoming/movie.mkv')), file);
  assert.equal(writes.length, beforeViewing + 4, 'Only the four explicit metadata refreshes may write');
  assert.ok(writes.slice(beforeViewing).every(([method, path]) => method === 'POST' && path === '/api/v1/metadata-refresh/commands'));
  assert.ok(blockedImages.length > 0, 'Failed-image path must actually be exercised');
  assert.deepEqual(external, []); assert.deepEqual(errors, []);
  console.log('PASS actual grouped credits add/read (105 rows), namesakes/multiple roles, blocked-image fallback, escaped text/safe links/mobile; actual refresh replacement with stable ID, absent preserves, malformed atomic rejection and empty clears; settings/files preserved and viewing read-only. Image failure, read-error, incomplete-page and delayed-response lifecycle cases use explicit interception; no external network.');
} finally {try {await mode(0);} finally {await browser.close();}}
