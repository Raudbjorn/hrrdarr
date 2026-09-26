// Opt-in real browser regression for the existing-library import wizard. Requires the owned
// library_ui_fixture and Vite proxy; see docs/library-ui.md#runnable-isolated-browser-verification.
//
// Run this against a FRESH fixture instance that no other browser scenario has touched: point
// UI_TV_ROOT/UI_MOVIE_ROOT at the fixture's own printed tv_path/movie_path. Do not reuse a
// fixture that library-browser.mjs already ran against -- it adds a series/movie whose path can
// overlap these seeded subfolders and every create below would then hit a real path conflict.
import assert from 'node:assert/strict';
import { mkdirSync, writeFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';
const { chromium } = await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE || '/usr/lib/node_modules/playwright/index.mjs').href);
const origin = process.env.UI_URL;
const tvPath = process.env.UI_TV_ROOT;
const moviePath = process.env.UI_MOVIE_ROOT;
assert.ok(origin && tvPath && moviePath, 'Set UI_URL, UI_TV_ROOT and UI_MOVIE_ROOT from your owned fixture (its printed tv_path/movie_path)');
assert.equal(new URL(origin).hostname, '127.0.0.1');

// Seed two TV folders (alphabetically Fixture < Second, so the second is the one the fixture's
// duplicate tvdb id 101 rejects) and one movie folder, each with a filename real rescan parsing
// recognizes: TV needs an SxxEyy token matching the fixture's season/episode 1; the movie needs
// its release year token.
mkdirSync(`${tvPath}/Fixture Import Show`, { recursive: true });
writeFileSync(`${tvPath}/Fixture Import Show/Fixture.Import.Show.S01E01.mkv`, Buffer.alloc(65536));
mkdirSync(`${tvPath}/Second Fixture Show`, { recursive: true });
writeFileSync(`${tvPath}/Second Fixture Show/Second.Fixture.Show.S01E01.mkv`, Buffer.alloc(65536));
mkdirSync(`${moviePath}/Fixture Import Movie`, { recursive: true });
writeFileSync(`${moviePath}/Fixture Import Movie/Fixture.Import.Movie.2021.1080p.WEB-DL.mkv`, Buffer.alloc(65536));

const browser = await chromium.launch({ headless: true });
const page = await browser.newPage();
page.setDefaultTimeout(15000);
const errors = [];
page.on('pageerror', error => errors.push(error.message));

await page.goto(origin);
await page.getByRole('button', { name: 'Import existing', exact: true }).click();
const panel = page.getByRole('region', { name: 'Import existing library' });
await panel.getByRole('heading', { name: 'Import existing library' }).waitFor();

async function addAndSelectRoot(path) {
  await panel.getByLabel('Add a root folder').fill(path);
  await panel.getByRole('button', { name: 'Add root folder', exact: true }).click();
  await panel.getByRole('button', { name: new RegExp(`^${path.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}`) }).click();
}

async function matchFolder(name, title) {
  const row = panel.locator('li.row').filter({ hasText: name });
  await row.getByRole('button', { name: `Search for ${name}`, exact: true }).click();
  await row.getByRole('button', { name: new RegExp(`^${title} `) }).click();
  await row.getByText(`Matched: ${title}`, { exact: false }).waitFor();
}

// --- TV: one clean match, one deliberate duplicate to force a 409 on the second create ---
await panel.getByRole('button', { name: 'TV', exact: true }).click();
await addAndSelectRoot(tvPath);
await panel.getByRole('heading', { name: tvPath, exact: true }).waitFor();
await matchFolder('Fixture Import Show', 'Fixture series');
await matchFolder('Second Fixture Show', 'Fixture series');
await panel.getByRole('button', { name: /^Review 2 selected folder/ }).click();
await panel.getByText('2 folder(s) will be created', { exact: false }).waitFor();
await panel.getByText('Another selected folder matches the same title', { exact: false }).first().waitFor();
await panel.getByRole('button', { name: 'Start import', exact: true }).click();

// First row must actually adopt its on-disk episode file.
const successRow = panel.locator('li.row').filter({ hasText: 'Fixture Import Show' });
await successRow.getByText('Done', { exact: true }).waitFor({ timeout: 20000 });
await successRow.getByText(/file\(s\) adopted/).waitFor();
assert.match(await successRow.innerText(), /1 file\(s\) adopted, 0 removed/);

// Second row (same tvdb id, different path) must fail as a definite conflict, not silently, and
// must not have blocked the first row above from completing.
const conflictRow = panel.locator('li.row').filter({ hasText: 'Second Fixture Show' });
await conflictRow.getByText('Failed to create', { exact: true }).waitFor();
assert.match(await conflictRow.innerText(), /409|conflict/i);
await conflictRow.getByRole('button', { name: /^Retry create for Second Fixture Show/ }).waitFor();

// The succeeded folder must disappear from the live unmapped scan; the failed one must remain.
// Exercise the UI's own refresh action, then verify against the real API response directly
// so this assertion does not depend on the panel's own choice to keep finished rows visible.
await panel.getByRole('button', { name: 'Refresh root folder observation', exact: true }).click();
await panel.getByText('Refreshing…').waitFor({ state: 'hidden' }).catch(() => {});
const rootsAfter = await (await page.request.get(`${origin}/api/v1/tv/root-folders`)).json();
const tvRoot = rootsAfter.items.find(r => r.path === tvPath);
assert.ok(tvRoot, 'TV root folder must still be registered');
assert.ok(tvRoot.unmapped_folders.some(f => f.name === 'Second Fixture Show'), 'Failed folder must still be unmapped on disk');
assert.ok(!tvRoot.unmapped_folders.some(f => f.name === 'Fixture Import Show'), 'Succeeded folder must no longer be unmapped on disk');

// --- Movies: single clean match, confirms the movie-domain path end to end ---
await page.getByRole('button', { name: 'Library', exact: true }).click();
await page.getByRole('button', { name: 'Import existing', exact: true }).click();
await panel.getByRole('button', { name: 'Movies', exact: true }).click();
await addAndSelectRoot(moviePath);
await panel.getByRole('heading', { name: moviePath, exact: true }).waitFor();
await matchFolder('Fixture Import Movie', 'Fixture movie');
await panel.getByRole('button', { name: /^Review 1 selected folder/ }).click();
await panel.getByRole('button', { name: 'Start import', exact: true }).click();
const movieRow = panel.locator('li.row').filter({ hasText: 'Fixture Import Movie' });
await movieRow.getByText('Done', { exact: true }).waitFor({ timeout: 20000 });
assert.match(await movieRow.innerText(), /1 file\(s\) adopted, 0 removed/);

assert.deepEqual(errors, [], `Uncaught page errors: ${errors.join('; ')}`);
console.log('import-existing-browser.mjs: all assertions passed');
await browser.close();
