// Owned fixture only. Real add/import/metadata endpoints and filesystem assertions.
// Explicit overlays cover a missing bulk target, lost accepted response, malformed readback,
// and delayed reads. Pagination and shared-TV associations use the opt-in actual fixture seed.
import assert from 'node:assert/strict';
import {mkdir, readFile, realpath, stat} from 'node:fs/promises';
import {basename, isAbsolute, join} from 'node:path';
import {pathToFileURL} from 'node:url';

const origin = process.env.UI_URL;
assert.equal(new URL(origin).hostname, '127.0.0.1');
assert.ok(process.env.UI_SCRATCH && isAbsolute(process.env.UI_SCRATCH));
const scratch = await realpath(process.env.UI_SCRATCH);
assert.ok(basename(scratch).startsWith('hrrdarr-library-ui-'));
const artifacts = process.env.UI_SCREENSHOT_DIR;
if (artifacts) {
  assert.ok(isAbsolute(artifacts) && artifacts.includes('/.do-not-commit/loop/iteration103'), 'Screenshots require an owned ignored iteration103 directory');
  await mkdir(artifacts, {recursive: true});
}
const {chromium} = await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE || '/usr/lib/node_modules/playwright/index.mjs').href);
const browser = await chromium.launch({headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? {executablePath: process.env.PLAYWRIGHT_EXECUTABLE} : {})});
const page = await browser.newPage({viewport: {width: 1280, height: 900}});
page.setDefaultTimeout(20000);
const errors = [], external = [], writes = [];
let expectedDialog = null;
page.on('pageerror', error => errors.push(error.message));
page.on('dialog', dialog => {
  if (!expectedDialog || dialog.type() !== 'confirm' || !/Discard unsaved file metadata changes/.test(dialog.message())) {
    errors.push(`Unexpected dialog: ${dialog.type()} ${dialog.message()}`); void dialog.dismiss(); return;
  }
  const accept = expectedDialog === 'discard'; expectedDialog = null;
  void (accept ? dialog.accept() : dialog.dismiss());
});
page.on('request', request => {
  if (!['GET', 'HEAD'].includes(request.method())) writes.push({method: request.method(), path: new URL(request.url()).pathname, body: request.postDataJSON()});
});
await page.route('**/*', route => {
  const url = new URL(route.request().url());
  if (['http:', 'https:'].includes(url.protocol) && url.hostname !== '127.0.0.1') {external.push(url.href); return route.abort();}
  return route.continue();
});
const panel = page.getByRole('region', {name: 'File metadata', exact: true});
const form = panel.getByRole('form', {name: 'Edit file metadata', exact: true});
const domains = page.getByRole('navigation', {name: 'Library type'});
const workspace = page.getByRole('navigation', {name: 'Workspace'});
const button = name => panel.getByRole('button', {name, exact: true});
const filePath = (domain, id) => `/api/v1/${domain}/files/${id}`;
async function get(path) {
  const response = await page.request.get(origin + path, {timeout: 10000});
  assert.equal(response.status(), 200, await response.text()); return response.json();
}
async function post(path, data, status = 202) {
  const response = await page.request.post(origin + path, {data, timeout: 20000});
  assert.equal(response.status(), status, await response.text()); return response.json();
}
async function until(read, predicate, label) {
  const deadline = Date.now() + 20000; let last;
  while (Date.now() < deadline) {last = await read(); if (predicate(last)) return last; await page.waitForTimeout(50);}
  assert.fail(`${label}: ${JSON.stringify(last)}`);
}
async function ready(control) {await control.waitFor(); await until(() => control.isEnabled(), Boolean, 'Disabled control');}
async function settled() {await until(() => panel.getAttribute('aria-busy'), value => value === 'false', 'File panel busy'); await ready(button('Reload file metadata'));}
async function choose(domain, title) {
  await domains.getByRole('button', {name: domain === 'tv' ? 'TV' : 'Movies', exact: true}).click();
  await page.getByRole('complementary', {name: 'Library collection'}).getByRole('button', {name: new RegExp(`^${title}`)}).click();
  await page.getByRole('article', {name: 'Library detail'}).getByRole('heading', {name: title, exact: true}).waitFor();
  await settled();
}
async function add(domain, title, directory) {
  await domains.getByRole('button', {name: domain === 'tv' ? 'TV' : 'Movies', exact: true}).click();
  await page.getByRole('button', {name: domain === 'tv' ? 'Add series' : 'Add movie', exact: true}).click();
  await page.getByLabel('Search catalogue', {exact: true}).fill('Fixture');
  await page.getByRole('button', {name: 'Search', exact: true}).click();
  await page.getByRole('button', {name: new RegExp(`^${title} `)}).click();
  await page.getByLabel('Existing library directory', {exact: true}).fill(directory);
  await page.getByRole('button', {name: 'Add to library', exact: true}).click();
  await page.getByRole('article', {name: 'Library detail'}).getByRole('heading', {name: title, exact: true}).waitFor();
  const result = (await get(domain === 'tv' ? '/api/v1/tv/series' : '/api/v1/movies')).items.find(item => item.title === title);
  assert.ok(result); return result;
}
async function importFile(media_type, id, source, destination) {
  const preview = await post('/api/v1/imports', {target: {media_type, id}, source, destination, mode: 'copy'});
  const complete = await post(`/api/v1/imports/${preview.id}/execute`, {}, 200);
  assert.equal(complete.status, 'complete');
}
async function begin(id) {await ready(button(`Edit file ${id}`)); await button(`Edit file ${id}`).click(); await form.waitFor();}
async function action(label, value) {await form.getByLabel(`${label} action`, {exact: true}).selectOption(value);}
async function save(expectedPatch, domain, idOrEditor) {
  const count = writes.length;
  await form.getByRole('button', {name: 'Save file metadata', exact: true}).click();
  await panel.getByText(/^Saved and read back metadata for /).waitFor();
  assert.equal(await form.count(), 0);
  const submitted = writes.slice(count).filter(row => row.method === 'PUT');
  assert.equal(submitted.length, 1, 'One explicit save produces one PUT');
  assert.equal(submitted[0].path, filePath(domain, idOrEditor));
  assert.deepEqual(submitted[0].body, expectedPatch);
}
async function disk(paths) {
  return Promise.all(paths.map(async path => {const info = await stat(path); return {path, dev: info.dev, ino: info.ino, bytes: (await readFile(path)).toString('base64')};}));
}
const unchangedFields = file => Object.fromEntries(['id', 'path', 'relative_path', 'size', 'date_added', 'series_id', 'movie_id', 'season_number', 'original_file_path', 'media_info'].filter(key => key in file).map(key => [key, file[key]]));
const metadataWrites = () => writes.filter(row => row.method === 'PUT' && /^\/api\/v1\/(tv|movies)\/files\//.test(row.path)).length;

try {
  await page.goto(origin);
  const tv = await add('tv', 'Fixture series', join(scratch, 'tv'));
  const movie = await add('movies', 'Fixture movie', join(scratch, 'movies'));
  const episodes = (await get(`/api/v1/episodes?series_id=${tv.id}`)).items;
  assert.equal(episodes.length, 2, 'Use a fresh fixture in metadata mode 0');
  await importFile('episode', episodes[0].id, join(scratch, 'incoming/episode.mkv'), join(scratch, 'tv/first.mkv'));
  const seeded = await post('/api/fixture/file-metadata-seed', {series_id: tv.id}, 200);
  assert.equal(seeded.series_id, tv.id); assert.equal(seeded.file_ids.length, 26);
  assert.ok(seeded.shared_episode_ids.length >= 2);
  await importFile('movie', movie.id, join(scratch, 'incoming/movie.mkv'), join(scratch, 'movies/movie.mkv'));
  const allTvFiles = (await get(`/api/v1/tv/files?series_id=${tv.id}&limit=500`)).items;
  const tvFiles = allTvFiles.slice(0, 2);
  const movieFile = (await get(`/api/v1/movies/files?movie_ids=${movie.id}`)).items[0];
  assert.equal(allTvFiles.length, 26); assert.equal(allTvFiles.filter(file => file.id === seeded.shared_file_id).length, 1);
  assert.equal(tvFiles[0].id, seeded.shared_file_id); assert.equal(tvFiles[0].id, movieFile.id, 'Fresh domain file IDs must collide');
  const beforeDisk = await disk([...allTvFiles.map(file => file.path), movieFile.path, join(scratch, 'incoming/episode.mkv'), join(scratch, 'incoming/movie.mkv')]);
  const beforeEpisodes = await get(`/api/v1/episodes?series_id=${tv.id}`);
  assert.deepEqual(beforeEpisodes.items.filter(episode => episode.episode_file_id === seeded.shared_file_id).map(episode => episode.id).sort((a, b) => a - b), [...seeded.shared_episode_ids].sort((a, b) => a - b));
  const repeatedSeed = await page.request.post(origin + '/api/fixture/file-metadata-seed', {data: {series_id: tv.id}, timeout: 10000});
  assert.equal(repeatedSeed.status(), 409, await repeatedSeed.text());
  assert.deepEqual((await get(`/api/v1/tv/files?series_id=${tv.id}&limit=500`)).items, allTvFiles, 'Rejected repeat seed must not add or alter files');
  assert.deepEqual(await get(`/api/v1/episodes?series_id=${tv.id}`), beforeEpisodes, 'Rejected repeat seed must not add associations');
  assert.deepEqual(await disk(beforeDisk.map(file => file.path)), beforeDisk);
  const beforeMovie = await get(`/api/v1/movies/${movie.id}`);
  const beforeTv = await get(`/api/v1/tv/series/${tv.id}`);
  const mutationStart = writes.length;

  // Every supported field uses actual UI controls and actual mutation/readback endpoints.
  for (const [domain, title, file] of [['tv', 'Fixture series', tvFiles[0]], ['movies', 'Fixture movie', movieFile]]) {
    await choose(domain, title); await begin(file.id);
    assert.equal(await form.getByLabel(domain === 'tv' ? 'Edition action' : 'Release type action', {exact: true}).count(), 0);
    await action('Quality', 'set');
    const quality = form.getByLabel(/^QualityChoose a quality/);
    // Actual DOM includes nested select option text in the label. Match its precise prefix,
    // excluding Quality action/revision, then wait before non-retrying option enumeration.
    await ready(quality);
    const qualityId = await quality.locator('option').evaluateAll(options => options.find(option => option.value !== '')?.value);
    assert.ok(qualityId); await quality.selectOption(qualityId);
    await form.getByLabel(/^Quality revisionReplace revision with unknown/).selectOption('known');
    await form.getByLabel('Revision version', {exact: true}).fill('2');
    await form.getByLabel('Revision real', {exact: true}).fill('0');
    await form.getByLabel('Repack', {exact: true}).check();
    await action('Languages', 'set'); await form.getByRole('button', {name: 'Clear language selection', exact: true}).click();
    await action('Release group', 'set'); await form.getByLabel('Release group', {exact: true}).fill('');
    await action('Indexer flags', 'set');
    const patch = {quality: {quality_id: Number(qualityId), revision: {version: 2, real: 0, is_repack: true}}, languages: [], release_group: '', indexer_flags: 0};
    if (domain === 'tv') {await action('Release type', 'set'); await form.getByLabel(/^Release typeChoose a release type/).selectOption('0'); patch.release_type = 0;}
    else {await action('Edition', 'set'); await form.getByLabel('Edition', {exact: true}).fill(''); patch.edition = '';}
    await save(patch, domain, file.id);
    const saved = await get(filePath(domain, file.id));
    for (const [key, value] of Object.entries(patch)) assert.deepEqual(saved[key], value);
    assert.deepEqual(unchangedFields(saved), unchangedFields(file));
    await begin(file.id); await action('Languages', 'set');
    await form.getByRole('group', {name: 'Concrete languages', exact: true}).getByLabel('Unknown', {exact: true}).check();
    await action('Quality', 'set'); await form.getByLabel(/^Quality revisionReplace revision with unknown/).selectOption('unknown');
    await action('Indexer flags', 'set');
    const flag = form.getByRole('group', {name: 'Indexer flags', exact: true}).locator('input[value="8"]');
    assert.match(await flag.locator('..').innerText(), domain === 'tv' ? /Internal/i : /Golden/i);
    await flag.check();
    await save({quality: {quality_id: Number(qualityId), revision: null}, languages: [0], indexer_flags: 8}, domain, file.id);
    const known = await get(filePath(domain, file.id)); assert.deepEqual(known.languages, [0]); assert.equal(known.quality.revision, null);
    await begin(file.id);
    for (const label of ['Quality', 'Languages', 'Release group', 'Indexer flags', domain === 'tv' ? 'Release type' : 'Edition']) await action(label, 'clear');
    await save(Object.fromEntries(Object.keys(patch).map(key => [key, null])), domain, file.id);
    const cleared = await get(filePath(domain, file.id));
    for (const key of Object.keys(patch)) assert.equal(cleared[key], null);
  }

  await choose('tv', 'Fixture series');
  for (const file of tvFiles) await panel.getByLabel(`Select file ${file.id}`, {exact: true}).check();
  await button('Edit selected files (2)').click(); await action('Release group', 'set');
  await form.getByLabel('Release group', {exact: true}).fill('共同 group');
  if (artifacts) {await form.scrollIntoViewIfNeeded(); await page.screenshot({path: join(artifacts, 'file-metadata-desktop.png'), fullPage: true}); await page.screenshot({path: join(artifacts, 'file-metadata-desktop-viewport.png')});}
  await save({file_ids: tvFiles.map(file => file.id), release_group: '共同 group'}, 'tv', 'editor');
  for (const file of tvFiles) assert.equal((await get(filePath('tv', file.id))).release_group, '共同 group');
  assert.equal((await get(filePath('movies', movieFile.id))).release_group, null, 'Colliding movie file ID must remain untouched');

  // Request overlay: forward the common editor write with a missing final ID to the REAL API.
  // The response and rollback are real; fixture membership failure is injected, not a UI-selected row.
  const editorRoute = url => url.pathname === filePath('tv', 'editor');
  await button('Edit selected files (2)').click(); await action('Release group', 'set');
  await form.getByLabel('Release group', {exact: true}).fill('must roll back');
  let rejected = false;
  await page.route(editorRoute, async route => {
    const body = route.request().postDataJSON();
    const response = await route.fetch({postData: {...body, file_ids: [...body.file_ids, 2147483647]}});
    assert.equal(response.status(), 404); rejected = true; await route.fulfill({response});
  });
  await form.getByRole('button', {name: 'Save file metadata', exact: true}).click();
  await panel.getByRole('alert').filter({hasText: 'One or more files do not exist'}).waitFor();
  assert.equal(rejected, true); assert.equal(await form.getByLabel('Release group', {exact: true}).inputValue(), 'must roll back');
  for (const file of tvFiles) assert.equal((await get(filePath('tv', file.id))).release_group, '共同 group');
  await page.unroute(editorRoute);
  expectedDialog = 'discard'; await button('Close metadata edit').click(); assert.equal(expectedDialog, null);

  // Dirty workspace navigation keeps the draft; owner changes require explicit keep/discard.
  await begin(tvFiles[0].id); await action('Release group', 'set'); await form.getByLabel('Release group', {exact: true}).fill('retained draft');
  await workspace.getByRole('button', {name: 'Health', exact: true}).click();
  await workspace.getByRole('button', {name: 'Library', exact: true}).click();
  assert.equal(await form.getByLabel('Release group', {exact: true}).inputValue(), 'retained draft');
  expectedDialog = 'keep'; await domains.getByRole('button', {name: 'Movies', exact: true}).click();
  assert.equal(expectedDialog, null); assert.equal(await form.getByLabel('Release group', {exact: true}).inputValue(), 'retained draft');
  expectedDialog = 'discard'; await domains.getByRole('button', {name: 'Movies', exact: true}).click();
  assert.equal(expectedDialog, null); await choose('movies', 'Fixture movie'); assert.equal(await form.count(), 0);

  // Actual filename preview is invalidated by a confirmed file save for this movie.
  const preview = page.getByRole('region', {name: 'Movie filename preview', exact: true});
  await preview.getByRole('button', {name: 'Preview filenames', exact: true}).click();
  await preview.getByText(/files considered/).waitFor();
  // movie_files.movie_id is unique: the movie owner has one selectable file, not a
  // fabricated multi-file collection. This exercises its selected-file entry point.
  await panel.getByLabel(`Select file ${movieFile.id}`, {exact: true}).check();
  await button('Edit selected files (1)').click();
  await action('Edition', 'set'); await form.getByLabel('Edition', {exact: true}).fill('Director cut');
  await save({edition: 'Director cut'}, 'movies', movieFile.id);
  await preview.getByRole('button', {name: 'Preview filenames', exact: true}).waitFor();
  assert.equal(await preview.getByText(/files considered/).count(), 0, 'Previous filename result must be invalidated');
  await preview.getByRole('button', {name: 'Preview filenames', exact: true}).click(); await preview.getByText(/files considered/).waitFor();

  // Commit an actual PUT, lose only its response, then exercise incomplete/wrong-owner readbacks.
  await choose('tv', 'Fixture series');
  for (const file of tvFiles) await panel.getByLabel(`Select file ${file.id}`, {exact: true}).check();
  await button('Edit selected files (2)').click(); await action('Release group', 'set'); await form.getByLabel('Release group', {exact: true}).fill('accepted response lost');
  let accepted = false;
  await page.route(editorRoute, async route => {const response = await route.fetch(); assert.equal(response.status(), 200); accepted = true; await route.abort('failed');});
  const beforeLost = metadataWrites();
  await form.getByRole('button', {name: 'Save file metadata', exact: true}).click();
  await panel.getByRole('alert').filter({hasText: 'The write outcome has not been fully reconciled'}).waitFor();
  assert.equal(accepted, true); await page.unroute(editorRoute);
  for (const file of tvFiles) assert.equal((await get(filePath('tv', file.id))).release_group, 'accepted response lost');
  // Parent lock propagation follows the panel's uncertainty render; immediate isDisabled does not retry.
  await until(() => domains.getByRole('button', {name: 'Movies', exact: true}).isDisabled(), Boolean, 'Uncertain metadata must lock owner changes');
  assert.equal(await domains.getByRole('button', {name: 'Movies', exact: true}).isDisabled(), true);
  assert.equal(await form.getByRole('button', {name: 'Save file metadata', exact: true}).isDisabled(), true);
  const readbackRoute = url => url.pathname === '/api/v1/tv/files' && url.searchParams.has('file_ids');
  for (const corrupt of [data => ({...data, items: data.items.slice(0, 1)}), data => ({...data, items: data.items.map(file => ({...file, series_id: tv.id + 1000}))})]) {
    await page.route(readbackRoute, async route => {const response = await route.fetch(); const data = await response.json(); await route.fulfill({response, json: corrupt(data)});});
    await button('Read back attempted files').click(); await ready(button('Read back attempted files'));
    assert.equal(await domains.getByRole('button', {name: 'Movies', exact: true}).isDisabled(), true);
    assert.equal(await button('Finish readback review').count(), 0);
    await page.unroute(readbackRoute);
  }
  await workspace.getByRole('button', {name: 'Health', exact: true}).click(); await workspace.getByRole('button', {name: 'Library', exact: true}).click();
  await button('Read back attempted files').click(); await button('Finish readback review').waitFor();
  assert.equal(await form.getByLabel('Release group', {exact: true}).inputValue(), 'accepted response lost');
  await panel.getByRole('region', {name: 'Current file metadata readback'}).getByText('accepted response lost', {exact: true}).first().waitFor();
  assert.equal(metadataWrites(), beforeLost + 1, 'Reconciliation must never repeat the PUT');
  await button('Finish readback review').click(); await settled();

  // Actual 26-row fixture: selection is page-local and the shared TV file appears once.
  await button('Reload file metadata').click(); await settled();
  assert.equal(await panel.getByRole('checkbox', {name: /^Select file /}).count(), 25);
  assert.equal(await panel.getByLabel(`Select file ${seeded.shared_file_id}`, {exact: true}).count(), 1);
  await panel.getByLabel(`Select file ${tvFiles[0].id}`, {exact: true}).check();
  await button('Next files').click(); await settled();
  // Wait for the new page's rendered membership before reading non-retrying counts.
  await panel.getByLabel(`Select file ${allTvFiles[25].id}`, {exact: true}).waitFor();
  assert.equal(await button('Edit selected files (0)').isDisabled(), true);
  assert.equal(await panel.getByRole('checkbox', {name: /^Select file /}).count(), 1);
  await panel.getByLabel(`Select file ${allTvFiles[25].id}`, {exact: true}).check();
  await button('Previous files').click(); await settled();
  assert.equal(await panel.getByLabel(`Select file ${tvFiles[0].id}`, {exact: true}).isChecked(), false);

  // Hold a real list response across an owner/domain change; delivering it must not replace movies.
  const pageRoute = url => url.pathname === '/api/v1/tv/files' && url.searchParams.get('series_id') === String(tv.id);
  let capture;
  const held = new Promise(resolve => {capture = resolve;});
  await page.route(pageRoute, async route => {const response = await route.fetch(); capture({route, response});});
  await button('Reload file metadata').click();
  let deadline;
  const delayed = await Promise.race([held, new Promise((_, reject) => {deadline = setTimeout(() => reject(new Error('No delayed file list')), 20000);})]).finally(() => clearTimeout(deadline));
  await choose('movies', 'Fixture movie');
  const delivered = page.waitForResponse(response => pageRoute(new URL(response.url())));
  await delayed.route.fulfill({response: delayed.response}); await (await delivered).finished(); await page.unroute(pageRoute);
  await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
  assert.equal(await panel.getByRole('checkbox', {name: /^Select file /}).count(), 1);
  await panel.getByText(`Fixture movie · Movie ${movie.id}`, {exact: true}).waitFor();

  // A movie's filename preview stays unavailable throughout an unresolved accepted edit.
  await preview.getByRole('button', {name: 'Preview filenames', exact: true}).click(); await preview.getByText(/files considered/).waitFor();
  await begin(movieFile.id); await action('Edition', 'set'); await form.getByLabel('Edition', {exact: true}).fill('Lost-response edition');
  const movieRoute = url => url.pathname === filePath('movies', movieFile.id);
  await page.route(movieRoute, async route => {
    if (route.request().method() !== 'PUT') return route.continue();
    const response = await route.fetch(); assert.equal(response.status(), 200); await route.abort('failed');
  });
  const beforeMovieLost = metadataWrites();
  await form.getByRole('button', {name: 'Save file metadata', exact: true}).click();
  await panel.getByRole('alert').filter({hasText: 'The write outcome has not been fully reconciled'}).waitFor();
  // The parent removes this child after the file panel publishes its uncertain state.
  await preview.waitFor({state: 'detached'});
  assert.equal(await preview.count(), 0, 'Uncertain metadata must not leave a filename preview available');
  await page.unroute(movieRoute);
  assert.equal((await get(filePath('movies', movieFile.id))).edition, 'Lost-response edition');
  await button('Read back attempted files').click(); await button('Finish readback review').waitFor();
  await preview.getByRole('button', {name: 'Preview filenames', exact: true}).waitFor();
  assert.equal(await preview.getByText(/files considered/).count(), 0);
  assert.equal(metadataWrites(), beforeMovieLost + 1);
  await button('Finish readback review').click();
  await choose('tv', 'Fixture series');

  // Mobile keyboard operation still goes through the actual single-file endpoint.
  await page.setViewportSize({width: 390, height: 844});
  assert.equal(await panel.evaluate(node => node.scrollWidth <= node.clientWidth + 1), true, 'File panel overflows mobile width');
  await button(`Edit file ${tvFiles[0].id}`).focus(); await page.keyboard.press('Enter'); await form.waitFor();
  await action('Release group', 'set'); await form.getByLabel('Release group', {exact: true}).fill('keyboard saved');
  await form.getByRole('button', {name: 'Save file metadata', exact: true}).focus(); await page.keyboard.press('Enter');
  await panel.getByText('Saved and read back metadata for 1 file.', {exact: true}).waitFor();
  assert.equal((await get(filePath('tv', tvFiles[0].id))).release_group, 'keyboard saved');
  await page.reload(); await choose('tv', 'Fixture series'); await button(`Inspect file ${tvFiles[0].id}`).click();
  await panel.getByRole('region', {name: `File ${tvFiles[0].id} facts`, exact: true}).getByText('keyboard saved', {exact: true}).waitFor();
  if (artifacts) {await panel.getByRole('region', {name: `File ${tvFiles[0].id} facts`, exact: true}).scrollIntoViewIfNeeded(); await page.screenshot({path: join(artifacts, 'file-metadata-mobile.png'), fullPage: true}); await page.screenshot({path: join(artifacts, 'file-metadata-mobile-viewport.png')});}
  assert.deepEqual(await disk(beforeDisk.map(file => file.path)), beforeDisk);
  assert.deepEqual(await get(`/api/v1/episodes?series_id=${tv.id}`), beforeEpisodes);
  assert.deepEqual(await get(`/api/v1/movies/${movie.id}`), beforeMovie);
  assert.deepEqual(await get(`/api/v1/tv/series/${tv.id}`), beforeTv);
  for (const file of allTvFiles) assert.deepEqual(unchangedFields(await get(filePath('tv', file.id))), unchangedFields(file));
  assert.deepEqual(unchangedFields(await get(filePath('movies', movieFile.id))), unchangedFields(movieFile));
  assert.ok(writes.slice(mutationStart).every(row => row.method === 'PUT' && /^\/api\/v1\/(tv|movies)\/files\/(editor|[1-9][0-9]*)$/.test(row.path)), 'Metadata flows must not submit library, monitoring, import, rename or other writes');
  assert.deepEqual(errors, []); assert.deepEqual(external, []);
  console.log('PASS actual TV/movie metadata edits, colliding file IDs, complete/unknown quality revision, null/empty/[0]/zero distinctions, real TV two-file common edits and rejected batch rollback, movie selected-file edition edit, dirty navigation, accepted PUT response loss with complete readback and no repeat, filename preview invalidation, mobile/keyboard and persisted reload; bytes/dev/inode/path/associations/monitoring unchanged. Actual 26-row pagination and shared-TV association use the bounded opt-in fixture seed; a repeat seed rejects409 without membership or filesystem changes. Request/readback/delayed-response fault cases use explicit overlays. The schema allows one file per movie owner: cross-movie bulk is not claimed. No forced process restart, external-writer CAS or physical rename/delete claimed.');
} finally {await browser.close();}
