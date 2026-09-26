import test from 'node:test';
import assert from 'node:assert/strict';
import { listRootFolders, getRootFolder, createRootFolder, findLibraryByExternalId, createRescanCommand, getRescanCommand } from '../src/lib/api.ts';

// These stubs verify client behavior for the existing-library import wizard's wrappers;
// Rust handler tests establish the wire specimens. Component/workflow behavior (sequential
// per-folder create+rescan, partial-failure handling, live status polling) is verified
// separately through the opt-in real-browser script, since this suite has no DOM/component
// rendering harness.
async function withFetch(stub, run) {
  const original = globalThis.fetch;
  globalThis.fetch = stub;
  try { await run(); } finally { globalThis.fetch = original; }
}
const json = (value, status = 200) => new Response(JSON.stringify(value), { status });

test('listRootFolders and getRootFolder target the domain-scoped collection', async () => {
  const page = { items: [{ id: 1, media_type: 'tv', path: '/tv', observation: 'available', accessible: true, writable: true, free_space: null, total_space: null, unmapped_folders: [{ name: 'Show', path: '/tv/Show', relative_path: 'Show' }] }], total: 1, limit: 50, offset: 0 };
  const paths = [];
  await withFetch(async (path, options) => { paths.push(path); assert.equal(options.method, undefined); return json(path.startsWith('/api/v1/tv/root-folders/') ? page.items[0] : page); }, async () => {
    assert.deepEqual(await listRootFolders('tv'), { ok: true, data: page });
    assert.deepEqual(await listRootFolders('movies', 50), { ok: true, data: page });
    assert.deepEqual(await getRootFolder('tv', 1), { ok: true, data: page.items[0] });
  });
  assert.deepEqual(paths, ['/api/v1/tv/root-folders?limit=50&offset=0', '/api/v1/movies/root-folders?limit=50&offset=50', '/api/v1/tv/root-folders/1']);
});

test('getRootFolder rejects invalid ids without fetching', async () => {
  let calls = 0;
  await withFetch(async () => { calls++; throw new Error('must not fetch'); }, async () => {
    for (const id of [0, -1, 1.5, NaN, Infinity]) assert.deepEqual(await getRootFolder('tv', id), { ok: false, error: 'ID must be a positive safe integer.' });
  });
  assert.equal(calls, 0);
});

test('createRootFolder posts the trimmed path and rejects an empty one locally', async () => {
  const created = { id: 5, media_type: 'movies', path: '/movies', observation: 'available', accessible: true, writable: true, free_space: null, total_space: null, unmapped_folders: [] };
  let calls = 0;
  await withFetch(async (path, options) => {
    calls++;
    assert.equal(path, '/api/v1/movies/root-folders');
    assert.equal(options.method, 'POST');
    assert.deepEqual(JSON.parse(options.body), { path: '/movies' });
    return json(created, 201);
  }, async () => assert.deepEqual(await createRootFolder('movies', '/movies'), { ok: true, data: created }));
  assert.equal(calls, 1);
  await withFetch(async () => { calls++; throw new Error('must not fetch'); }, async () => {
    assert.deepEqual(await createRootFolder('tv', ''), { ok: false, error: 'Root folder path is required.' });
    assert.deepEqual(await createRootFolder('tv', '   '), { ok: false, error: 'Root folder path is required.' });
  });
  assert.equal(calls, 1);
});

test('createRootFolder surfaces a structured conflict code for an existing path', async () => {
  await withFetch(async () => json({ error: { code: 'root_exists', message: 'root_exists' } }, 409), async () => {
    const result = await createRootFolder('tv', '/tv');
    assert.equal(result.ok, false);
    assert.equal(result.code, 'root_exists');
  });
});

test('findLibraryByExternalId queries the correct catalogue id field per domain', async () => {
  const page = { items: [{ id: 42, media_type: 'tv', metadata_id: null, title: 'Fixture series', year: null, tvdb_id: 101, tmdb_id: null, imdb_id: null, path: '/tv/x', poster: null, monitored: true, settings: {}, statistics: {}, is_available: null }], total: 1, limit: 1, offset: 0 };
  const paths = [];
  await withFetch(async (path) => { paths.push(path); return json(page); }, async () => {
    assert.deepEqual(await findLibraryByExternalId('tv', 101), { ok: true, data: page });
    assert.deepEqual(await findLibraryByExternalId('movies', 101), { ok: true, data: page });
  });
  assert.deepEqual(paths, ['/api/v1/tv/series?limit=1&tvdb_id=101', '/api/v1/movies?limit=1&tmdb_id=101']);
  let calls = 0;
  await withFetch(async () => { calls++; throw new Error('must not fetch'); }, async () => {
    for (const id of [0, -1, 1.5, NaN]) assert.equal((await findLibraryByExternalId('tv', id)).ok, false);
  });
  assert.equal(calls, 0);
});

test('createRescanCommand defaults to normal priority and validates the target id', async () => {
  const batch = { commands: [{ id: '11111111-1111-1111-1111-111111111111', media_type: 'tv', series_id: 9, movie_id: null, priority: 'normal', status: 'queued', attempts: 0, next_attempt_at: 0, created_at: 0, started_at: null, completed_at: null, error_code: null, skip_reason: null, files_adopted: null, files_removed: null }], busy_target_ids: [] };
  const bodies = [];
  await withFetch(async (path, options) => {
    assert.equal(path, '/api/v1/tv/rescan-commands');
    assert.equal(options.method, 'POST');
    bodies.push(JSON.parse(options.body));
    return json(batch, 202);
  }, async () => {
    assert.deepEqual(await createRescanCommand('tv', 9), { ok: true, data: batch });
    assert.deepEqual(await createRescanCommand('tv', 9, 'high'), { ok: true, data: batch });
  });
  assert.deepEqual(bodies, [{ target_id: 9, priority: 'normal' }, { target_id: 9, priority: 'high' }]);
  let calls = 0;
  await withFetch(async () => { calls++; throw new Error('must not fetch'); }, async () => {
    for (const id of [0, -1, 1.5, NaN]) assert.equal((await createRescanCommand('tv', id)).ok, false);
  });
  assert.equal(calls, 0);
});

test('getRescanCommand reads the scoped command and rejects a malformed id', async () => {
  const command = { id: '11111111-1111-1111-1111-111111111111', media_type: 'movies', series_id: null, movie_id: 3, priority: 'normal', status: 'succeeded', attempts: 1, next_attempt_at: 0, created_at: 0, started_at: 0, completed_at: 1, error_code: null, skip_reason: null, files_adopted: 1, files_removed: 0 };
  await withFetch(async (path) => { assert.equal(path, `/api/v1/movies/rescan-commands/${command.id}`); return json(command); }, async () => {
    assert.deepEqual(await getRescanCommand('movies', command.id), { ok: true, data: command });
  });
  let calls = 0;
  await withFetch(async () => { calls++; throw new Error('must not fetch'); }, async () => {
    assert.equal((await getRescanCommand('tv', 'not-a-uuid')).ok, false);
  });
  assert.equal(calls, 0);
});
