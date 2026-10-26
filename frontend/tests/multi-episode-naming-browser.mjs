// Real naming APIs in an owned library fixture. One held GET tests stale-preview rejection.
import assert from 'node:assert/strict';
import {mkdir, realpath} from 'node:fs/promises';
import {basename, isAbsolute, join} from 'node:path';
import {pathToFileURL} from 'node:url';

const origin = process.env.UI_URL;
assert.equal(new URL(origin).hostname, '127.0.0.1');
assert.ok(process.env.UI_SCRATCH && isAbsolute(process.env.UI_SCRATCH));
assert.ok(basename(await realpath(process.env.UI_SCRATCH)).startsWith('hrrdarr-library-ui-'));
const artifacts = process.env.UI_SCREENSHOT_DIR;
if (artifacts) {
  assert.ok(isAbsolute(artifacts) && artifacts.includes('/.do-not-commit/loop/iteration104'));
  await mkdir(artifacts, {recursive: true});
}
const {chromium} = await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE || '/usr/lib/node_modules/playwright/index.mjs').href);
const browser = await chromium.launch({headless: true, ...(process.env.PLAYWRIGHT_EXECUTABLE ? {executablePath: process.env.PLAYWRIGHT_EXECUTABLE} : {})});
const page = await browser.newPage({viewport: {width: 1280, height: 900}});
page.setDefaultTimeout(20000);
const errors = [], external = [], writes = [];
let releaseHeld;
page.on('pageerror', error => errors.push(error.message));
page.on('request', request => {
  if (!['GET', 'HEAD'].includes(request.method())) writes.push({method: request.method(), path: new URL(request.url()).pathname});
});
await page.route('**/*', route => {
  const url = new URL(route.request().url());
  if (['http:', 'https:'].includes(url.protocol) && url.hostname !== '127.0.0.1') { external.push(url.href); return route.abort(); }
  return route.continue();
});
const panel = page.getByRole('region', {name: 'Naming', exact: true});
const style = panel.getByRole('combobox', {name: /^Multi-episode style/});
const rename = panel.getByRole('checkbox', {name: 'Rename on automated import', exact: true});
const endpoint = '/api/v1/tv/config/naming/examples';
async function until(read, predicate, label) {
  const deadline = Date.now() + 20000;
  let last;
  while (Date.now() < deadline) { last = await read(); if (predicate(last)) return last; await page.waitForTimeout(50); }
  assert.fail(`${label}: ${JSON.stringify(last)}`);
}
async function preview(action, expectedStyle) {
  const response = page.waitForResponse(response => {
    const url = new URL(response.url());
    return url.pathname === endpoint && url.searchParams.get('multi_episode_draft') === 'true'
      && url.searchParams.get('multi_episode_style') === expectedStyle;
  });
  await action();
  const result = await response;
  assert.equal(result.status(), 200, await result.text());
  const data = await result.json();
  assert.equal(data.multi_episode_examples.length, 6);
  await until(() => panel.locator('.multi-examples dd').count(), count => count === 6, 'Six displayed examples');
  return data.multi_episode_examples;
}
async function checkFilenames(examples) {
  await until(() => panel.locator('.multi-examples dd').allTextContents(), texts =>
    examples.every((example, index) => !example.filename || texts[index]?.includes(example.filename)), 'Current filenames displayed');
}
try {
  await page.goto(origin);
  await page.getByRole('navigation', {name: 'Workspace'}).getByRole('button', {name: 'Naming', exact: true}).click();
  await style.waitFor();
  await rename.check();
  await panel.getByLabel('Standard episode format', {exact: true}).fill('{Series Title} - S{season:00}E{episode:00} - {Episode Title}');
  await panel.getByLabel('Daily episode format', {exact: true}).fill('{Series Title} - {Air-Date} - {Episode Title}');
  await panel.getByLabel('Anime episode format', {exact: true}).fill('{Series Title} - {absolute:000} - {Episode Title}');
  for (const value of ['0', '1', '2', '3', '4', '5']) {
    const examples = await preview(() => style.selectOption(value), value);
    assert.ok(examples.every(example => example.status === 'rendered'));
    await checkFilenames(examples);
  }
  assert.equal(writes.length, 0, 'Preview never changes saved settings');
  const save = panel.getByRole('button', {name: 'Save naming settings', exact: true});
  const saved = page.waitForResponse(response => response.request().method() === 'PUT' && new URL(response.url()).pathname === '/api/v1/tv/config/naming');
  await save.click();
  assert.equal((await saved).status(), 200);
  await until(() => save.isDisabled(), Boolean, 'Save complete');
  await page.reload();
  await page.getByRole('navigation', {name: 'Workspace'}).getByRole('button', {name: 'Naming', exact: true}).click();
  await until(() => style.inputValue(), value => value === '5', 'Saved style restored');
  const unset = await preview(() => style.selectOption(''), null);
  assert.ok(unset.every(example => example.status === 'needs_policy'), 'Explicit empty draft cannot fall back to saved style5');
  const cleared = await preview(() => panel.getByLabel('Standard episode format', {exact: true}).fill(''), null);
  assert.ok(cleared.filter(example => example.field === 'standard_episode_format').every(example => example.status === 'preserved'), 'Cleared template cannot fall back to its saved value');
  await preview(() => panel.getByLabel('Standard episode format', {exact: true}).fill('{Series Title} - S{season:00}E{episode:00} - {Episode Title}'), null);
  const preserved = await preview(() => rename.uncheck(), null);
  assert.ok(preserved.every(example => example.status === 'preserved'));
  await checkFilenames(preserved);
  await preview(() => rename.check(), null);
  await preview(() => style.selectOption('0'), '0');
  let heldReady, delivered = false;
  const ready = new Promise(resolve => { heldReady = resolve; });
  const gate = new Promise(resolve => { releaseHeld = resolve; });
  const hold = async route => {
    const url = new URL(route.request().url());
    if (url.searchParams.get('multi_episode_style') !== '3') return route.continue();
    const response = await route.fetch();
    assert.equal(response.status(), 200);
    heldReady();
    await gate;
    await route.fulfill({response});
    delivered = true;
  };
  await page.route('**/api/v1/tv/config/naming/examples?*', hold);
  await style.selectOption('3');
  await Promise.race([ready, page.waitForTimeout(20000).then(() => { throw new Error('Held preview did not arrive'); })]);
  const current = await preview(() => style.selectOption('4'), '4');
  await checkFilenames(current);
  releaseHeld();
  await until(() => delivered, Boolean, 'Old preview delivered');
  await page.unroute('**/api/v1/tv/config/naming/examples?*', hold);
  await checkFilenames(current);
  assert.equal(writes.length, 1, 'Reload and previews never resave settings');
  await page.setViewportSize({width: 390, height: 844});
  await style.scrollIntoViewIfNeeded();
  await style.focus();
  await page.keyboard.press('ArrowUp');
  await until(() => style.inputValue(), value => value === '3', 'Keyboard style selection');
  await until(() => panel.locator('.multi-examples dd').count(), count => count === 6, 'Keyboard preview');
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), true);
  if (artifacts) await page.screenshot({path: join(artifacts, 'multi-episode-naming-mobile.png')});
  assert.deepEqual(errors, []);
  assert.deepEqual(external, []);
  console.log(JSON.stringify({passed: true, modes: 6, examplesPerMode: 6, writes, overlay: 'One real GET held until a newer draft preview completed; response unchanged.'}));
} finally {
  releaseHeld?.();
  await browser.close();
}
