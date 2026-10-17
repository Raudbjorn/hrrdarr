import test from 'node:test';
import assert from 'node:assert/strict';
import {
  createDownloadedScanCommand, listDownloadedScanCommands, getDownloadedScanCommand,
  listDownloadedScanChildren, cancelDownloadedScanCommand,
} from '../src/lib/api.ts';

const id = 'c0000000-0000-4000-8000-000000000001';
const provider = 'd0000000-0000-4000-8000-000000000001';
const hash = 'ab'.repeat(20);
async function withFetch(fetch, run) {
  const original = globalThis.fetch;
  globalThis.fetch = fetch;
  try { await run(); } finally { globalThis.fetch = original; }
}
const response = (data, status = 200) => new Response(JSON.stringify(data), {status});

test('scan requests are domain scoped, keep the client UUID and never add a second POST', async () => {
  const calls = [];
  await withFetch(async (path, options) => {
    assert.ok(options.signal instanceof AbortSignal);
    calls.push({path, method: options.method, body: options.body && JSON.parse(options.body)});
    return response({items: [], total: 0, limit: 25, offset: 0});
  }, async () => {
    const input = {id, path: '/downloads/Harbor.S01E01.mkv', mode: 'auto', priority: 'normal'};
    assert.equal((await createDownloadedScanCommand('tv', input)).ok, true);
    assert.equal((await createDownloadedScanCommand('movies', {...input, tracked: {provider_id: provider, remote_id: hash}})).ok, true);
    assert.equal((await getDownloadedScanCommand('movies', id)).ok, true);
    assert.equal((await listDownloadedScanCommands('tv', {status: 'running', limit: 100, offset: 100000})).ok, true);
    assert.equal((await listDownloadedScanChildren('tv', id, {disposition: 'ready', limit: 5})).ok, true);
    assert.equal((await cancelDownloadedScanCommand('tv', id)).ok, true);
  });
  assert.deepEqual(calls, [
    {path: '/api/v1/tv/downloaded-scan-commands', method: 'POST', body: {id, path: '/downloads/Harbor.S01E01.mkv', mode: 'auto', priority: 'normal'}},
    {path: '/api/v1/movies/downloaded-scan-commands', method: 'POST', body: {id, path: '/downloads/Harbor.S01E01.mkv', mode: 'auto', priority: 'normal', tracked: {provider_id: provider, remote_id: hash}}},
    {path: `/api/v1/movies/downloaded-scan-commands/${id}`, method: undefined, body: undefined},
    {path: '/api/v1/tv/downloaded-scan-commands?status=running&limit=100&offset=100000', method: undefined, body: undefined},
    {path: `/api/v1/tv/downloaded-scan-commands/${id}/children?disposition=ready&limit=5`, method: undefined, body: undefined},
    {path: `/api/v1/tv/downloaded-scan-commands/${id}/cancel`, method: 'POST', body: undefined},
  ]);
});

test('invalid scan input never reaches the network', async () => {
  await withFetch(() => assert.fail('Invalid input reached fetch'), async () => {
    const good = {id, path: '/downloads/a.mkv', mode: 'auto', priority: 'normal'};
    const paths = ['', '/', 'relative.mkv', '/a/', '/a//b', '/a/./b', '/a/../b', '/a\0b', `/${'a'.repeat(4096)}`, '/ä'.repeat(2048)];
    for (const path of paths) assert.equal((await createDownloadedScanCommand('tv', {...good, path})).ok, false, JSON.stringify(path));
    for (const patch of [{id: 'not-a-uuid'}, {id: id.toUpperCase()}, {mode: 'hardlink'}, {priority: 'urgent'},
      {tracked: {provider_id: provider, remote_id: 'A'.repeat(40)}}, {tracked: {provider_id: 'nope', remote_id: hash}},
      {tracked: {provider_id: provider, remote_id: 'ab'}}]) {
      assert.equal((await createDownloadedScanCommand('tv', {...good, ...patch})).ok, false, JSON.stringify(patch));
    }
    assert.equal((await createDownloadedScanCommand('shows', good)).ok, false);
    for (const bad of ['', '../x', id.toUpperCase(), 'not-a-uuid']) {
      assert.equal((await getDownloadedScanCommand('tv', bad)).ok, false);
      assert.equal((await cancelDownloadedScanCommand('tv', bad)).ok, false);
      assert.equal((await listDownloadedScanChildren('tv', bad)).ok, false);
    }
    for (const query of [{limit: 0}, {limit: 101}, {limit: 1.5}, {offset: -1}, {offset: 100001}]) {
      assert.equal((await listDownloadedScanCommands('tv', query)).ok, false, JSON.stringify(query));
      assert.equal((await listDownloadedScanChildren('tv', id, query)).ok, false, JSON.stringify(query));
    }
  });
});

test('lost admission and cancel responses are returned without automatic resubmission', async () => {
  const input = {id, path: '/downloads/a.mkv', mode: 'copy', priority: 'high'};
  for (const invoke of [() => createDownloadedScanCommand('tv', input), () => cancelDownloadedScanCommand('tv', id)]) {
    let calls = 0;
    await withFetch(async () => { calls++; throw new Error('Response lost'); }, async () => {
      assert.deepEqual(await invoke(), {ok: false, error: 'Response lost'});
    });
    assert.equal(calls, 1);
  }
});

test('native conflicts keep their static code and HTTP status for explicit readback', async () => {
  await withFetch(async () => response({error: {code: 'downloaded_scan_conflict', message: 'downloaded_scan_conflict'}}, 409), async () => {
    assert.deepEqual(await createDownloadedScanCommand('tv', {id, path: '/downloads/a.mkv', mode: 'auto', priority: 'normal'}), {
      ok: false, status: 409, code: 'downloaded_scan_conflict', error: 'downloaded_scan_conflict',
    });
  });
});
