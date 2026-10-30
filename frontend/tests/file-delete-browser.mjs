// Owned fixture only: real add/import/settings/delete/health/cleanup endpoints, real command worker,
// and filesystem assertions inside the fixture scratch directory. No response overlays.
import assert from 'node:assert/strict';
import {mkdir, readFile, realpath, stat} from 'node:fs/promises';
import {basename, isAbsolute, join, relative} from 'node:path';
import {pathToFileURL} from 'node:url';

const origin = process.env.UI_URL;
assert.equal(new URL(origin).hostname, '127.0.0.1');
assert.ok(process.env.UI_SCRATCH && isAbsolute(process.env.UI_SCRATCH));
const scratch = await realpath(process.env.UI_SCRATCH);
assert.ok(basename(scratch).startsWith('hrrdarr-library-ui-'));
const artifacts = process.env.UI_SCREENSHOT_DIR;
if (artifacts) {
  assert.ok(isAbsolute(artifacts) && /\/\.do-not-commit\/loop\/iteration\d+/.test(artifacts), 'Screenshots require an owned ignored loop iteration directory');
  await mkdir(artifacts, {recursive: true});
}
const {chromium} = await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE || '/usr/lib/node_modules/playwright/index.mjs').href);
const browser = await chromium.launch({headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? {executablePath: process.env.PLAYWRIGHT_EXECUTABLE} : {})});
const page = await browser.newPage({viewport: {width: 1280, height: 900}});
page.setDefaultTimeout(20000);
const errors = [], external = [], writes = [];
page.on('pageerror', error => errors.push(error.message));
page.on('dialog', dialog => {errors.push(`Unexpected dialog: ${dialog.type()} ${dialog.message()}`); void dialog.dismiss();});
page.on('request', request => {
  if (!['GET', 'HEAD'].includes(request.method())) writes.push({method: request.method(), path: new URL(request.url()).pathname, body: request.postData()});
});
await page.route('**/*', route => {
  const url = new URL(route.request().url());
  if (['http:', 'https:'].includes(url.protocol) && url.hostname !== '127.0.0.1') {external.push(url.href); return route.abort();}
  return route.continue();
});
const files = page.getByRole('region', {name: 'File metadata', exact: true});
const bin = page.getByRole('region', {name: 'Recycle bin', exact: true});
const health = page.getByRole('region', {name: 'Health', exact: true});
const domains = page.getByRole('navigation', {name: 'Library type'});
const workspace = page.getByRole('navigation', {name: 'Workspace'});
const dialog = files.getByRole('alertdialog');
async function get(path) {
  const response = await page.request.get(origin + path, {timeout: 10000});
  assert.equal(response.status(), 200, await response.text()); return response.json();
}
async function post(path, data, status = 202) {
  const response = await page.request.post(origin + path, {data, timeout: 20000});
  assert.equal(response.status(), status, await response.text()); return response.json();
}
async function until(read, predicate, label) {
  const deadline = Date.now() + 25000; let last;
  while (Date.now() < deadline) {last = await read(); if (predicate(last)) return last; await page.waitForTimeout(50);}
  assert.fail(`${label}: ${JSON.stringify(last)}`);
}
const exists = path => stat(path).then(() => true, error => {if (error.code === 'ENOENT') return false; throw error;});
async function idle(region) {await until(() => region.getAttribute('aria-busy'), value => value === 'false', 'Region busy');}
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
  assert.equal((await post(`/api/v1/imports/${preview.id}/execute`, {}, 200)).status, 'complete');
}
async function choose(domain, title) {
  await workspace.getByRole('button', {name: 'Library', exact: true}).click();
  await domains.getByRole('button', {name: domain === 'tv' ? 'TV' : 'Movies', exact: true}).click();
  await page.getByRole('complementary', {name: 'Library collection'}).getByRole('button', {name: new RegExp(`^${title}`)}).click();
  await page.getByRole('article', {name: 'Library detail'}).getByRole('heading', {name: title, exact: true}).waitFor();
  await idle(files);
}
async function saveBin(domain, path, days) {
  await workspace.getByRole('button', {name: 'Media management', exact: true}).click();
  await idle(bin);
  const tab = bin.getByRole('navigation', {name: 'Recycle bin media'}).getByRole('button', {name: domain === 'tv' ? 'TV' : 'Movies', exact: true});
  if (await tab.getAttribute('aria-pressed') !== 'true') {await tab.click(); await idle(bin);}
  await bin.getByLabel('Recycle bin folder', {exact: true}).fill(path);
  await bin.getByLabel('Cleanup after (days)', {exact: true}).fill(String(days));
  await bin.getByRole('button', {name: 'Save recycle bin settings', exact: true}).click();
  await bin.getByText(path ? /^Recycle bin saved\./ : /^Recycle bin cleared\./).waitFor();
  const saved = await get(`/api/v1/${domain}/recycle-bin`);
  assert.equal(saved.path, path || null); assert.equal(saved.cleanup_days, days);
  return saved;
}
async function confirmDeletion(ids, expectedConfirm) {
  await dialog.waitFor();
  assert.equal(await dialog.getByRole('button', {name: expectedConfirm, exact: true}).evaluate(node => node === document.activeElement), true, 'Confirmation focuses the destructive action');
  for (const id of ids) await dialog.getByText(new RegExp(`^File ${id}: `)).waitFor();
  const before = writes.length;
  await dialog.getByRole('button', {name: expectedConfirm, exact: true}).click();
  await dialog.waitFor({state: 'detached'});
  const sent = writes.slice(before).filter(row => row.method === 'DELETE');
  assert.equal(sent.length, 1, 'One confirmation sends exactly one DELETE');
  return sent[0];
}
async function healthIssue(domain, reason) {
  await workspace.getByRole('button', {name: 'Health', exact: true}).click(); await idle(health);
  await health.getByLabel('Health scope').selectOption(domain); await idle(health);
  await health.getByRole('button', {name: `Run ${domain} health checks`, exact: true}).click();
  const row = health.getByRole('article', {name: `${domain}:recycle_bin`, exact: true});
  const want = reason ? new RegExp(`^error: .*\\(${reason}\\)$`) : /^No cached issue on this check/;
  await until(async () => {await idle(health); await health.getByRole('button', {name: 'Refresh health', exact: true}).click(); await idle(health); return row.getByText(want).count();}, count => count === 1, `Health ${domain} recycle_bin ${reason ?? 'restored'}`);
  return row;
}

try {
  await page.goto(origin);
  const tvBin = join(scratch, 'recycle-tv'), missingBin = join(scratch, 'recycle-movies');
  await mkdir(tvBin);
  const tv = await add('tv', 'Fixture series', join(scratch, 'tv'));
  const movie = await add('movies', 'Fixture movie', join(scratch, 'movies'));
  const episodes = (await get(`/api/v1/episodes?series_id=${tv.id}`)).items;
  assert.equal(episodes.length, 2, 'Use a fresh fixture in metadata mode 0');
  await importFile('episode', episodes[0].id, join(scratch, 'incoming/episode.mkv'), join(scratch, 'tv/first.mkv'));
  // Real seed: one TV file shared by two episodes plus 25 single-episode files.
  const seeded = await post('/api/fixture/file-metadata-seed', {series_id: tv.id}, 200);
  await importFile('movie', movie.id, join(scratch, 'incoming/movie.mkv'), join(scratch, 'movies/movie.mkv'));
  const tvFiles = (await get(`/api/v1/tv/files?series_id=${tv.id}&limit=500`)).items;
  const movieFile = (await get(`/api/v1/movies/files?movie_ids=${movie.id}`)).items[0];
  assert.equal(tvFiles.length, 26); assert.equal(seeded.shared_file_id, movieFile.id, 'Fresh domain file IDs must collide');
  const shared = tvFiles.find(file => file.id === seeded.shared_file_id);
  const sharedBytes = await readFile(shared.path), sharedInode = (await stat(shared.path)).ino;

  // TV: configure a bin through the UI, then recycle the shared multi-episode file once.
  await saveBin('tv', tvBin, 7);
  await choose('tv', 'Fixture series');
  await files.getByRole('button', {name: `Delete file ${shared.id}`, exact: true}).click();
  await dialog.getByRole('heading', {name: `Move file ${shared.id} to the recycle bin?`, exact: true}).waitFor();
  await dialog.getByText(new RegExp(`moved to ${tvBin.replaceAll('/', '\\/')}, inside a folder named after the series or movie`)).waitFor();
  // A pending confirmation locks the library owner. The colliding movie ID must remain untouched.
  const moviesTab = domains.getByRole('button', {name: 'Movies', exact: true});
  assert.equal(await moviesTab.isDisabled(), true, 'Pending deletion blocks a media-domain switch');
  assert.ok(await exists(movieFile.path), 'The colliding movie file remains before confirmation');
  // Escape and Cancel both leave everything untouched.
  await page.keyboard.press('Escape'); await files.getByText('Deletion cancelled; nothing was changed.', {exact: true}).waitFor();
  assert.ok(await exists(shared.path));
  await files.getByRole('button', {name: `Delete file ${shared.id}`, exact: true}).click();
  await dialog.getByRole('button', {name: 'Cancel deletion', exact: true}).click(); assert.ok(await exists(shared.path));
  assert.equal(writes.filter(row => row.method === 'DELETE').length, 0, 'Cancelling must not send a DELETE');
  if (artifacts) {await files.getByRole('button', {name: `Delete file ${shared.id}`, exact: true}).click(); await dialog.waitFor(); await dialog.scrollIntoViewIfNeeded(); await page.screenshot({path: join(artifacts, 'file-delete-recycle-confirm.png')}); await dialog.getByRole('button', {name: 'Cancel deletion', exact: true}).click();}
  await files.getByRole('button', {name: `Delete file ${shared.id}`, exact: true}).click();
  const single = await confirmDeletion([shared.id], `Move file ${shared.id} to recycle bin`);
  assert.equal(single.path, `/api/v1/tv/files/${shared.id}`); assert.deepEqual(JSON.parse(single.body), {mode: 'recycle'});
  const recycled = join(tvBin, 'tv', relative(join(scratch, 'tv'), shared.path));
  await files.getByText(`Moved file ${shared.id} to the recycle bin: ${recycled}.`, {exact: true}).waitFor();
  assert.equal(await exists(shared.path), false);
  assert.deepEqual(await readFile(recycled), sharedBytes); assert.equal((await stat(recycled)).ino, sharedInode, 'Same-device recycling is a rename');
  const afterEpisodes = (await get(`/api/v1/episodes?series_id=${tv.id}`)).items;
  for (const id of seeded.shared_episode_ids) assert.equal(afterEpisodes.find(row => row.id === id).episode_file_id, null, `episode ${id} detached`);
  assert.equal((await get(`/api/v1/tv/files?series_id=${tv.id}&limit=500`)).items.length, 25);
  assert.equal((await get(`/api/v1/movies/files?movie_ids=${movie.id}`)).items[0].id, movieFile.id, 'Colliding movie file id untouched');

  // TV bulk: two selected seeded files, one DELETE carrying both ids and the confirmed mode.
  await idle(files);
  const pair = (await get(`/api/v1/tv/files?series_id=${tv.id}&limit=2`)).items;
  for (const file of pair) await files.getByLabel(`Select file ${file.id}`, {exact: true}).check();
  await files.getByRole('button', {name: 'Delete selected files (2)', exact: true}).click();
  await dialog.getByRole('heading', {name: 'Move 2 files to the recycle bin?', exact: true}).waitFor();
  const bulk = await confirmDeletion(pair.map(file => file.id), 'Move 2 files to recycle bin');
  assert.equal(bulk.path, '/api/v1/tv/files/bulk'); assert.deepEqual(JSON.parse(bulk.body), {file_ids: pair.map(file => file.id), mode: 'recycle'});
  await files.getByText('Moved 2 files to the recycle bin.', {exact: true}).waitFor();
  for (const file of pair) {assert.equal(await exists(file.path), false); assert.ok(await exists(join(tvBin, 'tv', relative(join(scratch, 'tv'), file.path))));}
  assert.equal((await get(`/api/v1/tv/files?series_id=${tv.id}&limit=500`)).items.length, 23);

  // Movies: no bin, so the confirmation is explicitly permanent.
  await choose('movies', 'Fixture movie');
  await files.getByRole('button', {name: `Delete file ${movieFile.id}`, exact: true}).click();
  await dialog.getByRole('heading', {name: `Permanently delete file ${movieFile.id}?`, exact: true}).waitFor();
  await dialog.getByText(/No recycle bin is configured for this media type\. The file is deleted from disk and cannot be restored\./).waitFor();
  if (artifacts) {await dialog.scrollIntoViewIfNeeded(); await page.screenshot({path: join(artifacts, 'file-delete-permanent-confirm.png')});}
  const permanent = await confirmDeletion([movieFile.id], `Permanently delete file ${movieFile.id}`);
  assert.equal(permanent.path, `/api/v1/movies/files/${movieFile.id}`); assert.deepEqual(JSON.parse(permanent.body), {mode: 'permanent'});
  await files.getByText(`Permanently deleted file ${movieFile.id}.`, {exact: true}).waitFor();
  assert.equal(await exists(movieFile.path), false);
  assert.equal((await get(`/api/v1/movies/files?movie_ids=${movie.id}`)).items.length, 0);
  assert.equal((await get(`/api/v1/tv/files?series_id=${tv.id}&limit=500`)).items.length, 23, 'Movie deletion leaves TV files');

  // Health: a configured but missing movie bin is an error; creating it restores the check.
  await saveBin('movies', missingBin, 0);
  const row = await healthIssue('movies', 'recycle_bin_missing');
  await row.getByText(`error: The configured recycling bin folder does not exist: ${missingBin}. File deletion is refused until it exists. (recycle_bin_missing)`, {exact: true}).waitFor();
  if (artifacts) {await row.scrollIntoViewIfNeeded(); await page.screenshot({path: join(artifacts, 'file-delete-health-missing-bin.png')});}
  await mkdir(missingBin);
  await healthIssue('movies', null);
  await healthIssue('tv', null);

  // Manual cleanup through the UI: nothing is old enough, so recycled files remain.
  await saveBin('tv', tvBin, 7);
  await bin.getByRole('button', {name: 'Clean up recycle bin now', exact: true}).click();
  await bin.getByLabel('Latest recycle bin cleanup').getByText('Manual cleanup removed 0 files and 0 empty folders; kept 0 symlink or special entries.', {exact: true}).waitFor();
  assert.ok(await exists(recycled));
  // Disabled cleanup cannot be started from the UI.
  await saveBin('movies', missingBin, 0);
  assert.equal(await bin.getByRole('button', {name: 'Clean up recycle bin now', exact: true}).isDisabled(), true);
  await bin.getByText('Cleanup is disabled (0 days).', {exact: true}).waitFor();

  // Mobile layout and keyboard confirmation of a TV recycle.
  await page.setViewportSize({width: 390, height: 844});
  await choose('tv', 'Fixture series');
  const next = (await get(`/api/v1/tv/files?series_id=${tv.id}&limit=1`)).items[0];
  await files.getByRole('button', {name: `Delete file ${next.id}`, exact: true}).focus(); await page.keyboard.press('Enter');
  await dialog.waitFor();
  assert.equal(await files.evaluate(node => node.scrollWidth <= node.clientWidth + 1), true, 'File panel overflows mobile width');
  if (artifacts) await page.screenshot({path: join(artifacts, 'file-delete-mobile-confirm.png')});
  await page.keyboard.press('Enter');
  await files.getByText(new RegExp(`^Moved file ${next.id} to the recycle bin: `)).waitFor();
  assert.equal(await exists(next.path), false);

  const deletes = writes.filter(row => row.method === 'DELETE');
  assert.equal(deletes.length, 4, 'Exactly one DELETE per confirmation');
  assert.deepEqual(errors, []); assert.deepEqual(external, []);
  console.log('PASS actual UI recycle-bin settings (TV path, movie missing/disabled), explicit recycle confirmation with Escape/Cancel no-ops, shared two-episode TV file recycled once by rename (inode kept, both episodes detached), bulk two-file recycle, permanent movie deletion with colliding file id, Health recycle_bin_missing error and restoration, manual cleanup command keeping recent files, mobile keyboard confirmation. No restart, cross-device or failure-injection claims here (see Rust tests).');
} finally {await browser.close();}
