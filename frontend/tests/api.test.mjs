import test from 'node:test';
import assert from 'node:assert/strict';
import { loadSeries, loadEpisodes, previewImport, listLibrary, getLibrary, lookupLibrary, addLibrary, updateLibrary, listEpisodes, monitorEpisode, previewManualImport, getImport, executeImport, listProviders, getProviderSchema, getProvider, createProvider, updateProvider, deleteProvider, testProvider } from '../src/lib/api.ts';

// These stubs verify client behavior; Rust handler tests establish the wire specimens.
async function withFetch(stub, run) {
  const original = globalThis.fetch;
  globalThis.fetch = stub;
  try { await run(); } finally { globalThis.fetch = original; }
}
const json = (value, status = 200) => new Response(JSON.stringify(value), { status });
const preview = { episode_id: 7, source: '/source.mkv', destination: '/tv/file.mkv', mode: 'copy' };

test('library wrappers preserve null facts and target the selected series', async () => {
  const series = [{ id: 1, title: 'Show', year: null, path: '/tv', poster: null }];
  const episodes = [{ id: 7, season: 0, number: 1, title: 'Special', file_path: null }];
  const paths = [];
  await withFetch(async (path, options) => {
    paths.push(path);
    assert.ok(options.signal instanceof AbortSignal);
    assert.equal(options.method, undefined);
    return json(path === '/api/v1/series' ? series : episodes);
  }, async () => {
    assert.deepEqual(await loadSeries(), { ok: true, data: series });
    assert.deepEqual(await loadEpisodes(1), { ok: true, data: episodes });
  });
  assert.deepEqual(paths, ['/api/v1/series', '/api/v1/series/1/episodes']);
  await withFetch(async () => json([]), async () => {
    assert.deepEqual(await loadSeries(), { ok: true, data: [] });
    assert.deepEqual(await loadEpisodes(1), { ok: true, data: [] });
  });
});

test('preview serializes the request and preserves the typed operation target', async () => {
  const operation = { id: '0d3a11d1-4aba-4dd7-913d-d4b50a9065b3', target: { media_type: 'episode', id: 7 }, status: 'preview', message: 'Ready' };
  await withFetch(async (path, options) => {
    assert.equal(path, '/api/v1/imports');
    assert.equal(options.method, 'POST');
    assert.equal(options.headers['content-type'], 'application/json');
    assert.deepEqual(JSON.parse(options.body), preview);
    return json(operation);
  }, async () => assert.deepEqual(await previewImport(preview), { ok: true, data: operation }));
});

test('HTTP failures support legacy and structured errors without treating them as data', async () => {
  for (const [body, status, message] of [
    [{ error: 'Legacy failure' }, 400, 'Legacy failure'],
    [{ error: { code: 'library_conflict', message: 'Conflict' } }, 409, 'Conflict'],
    [{ unrelated: true }, 503, 'API request failed (503).'],
  ]) {
    await withFetch(async () => json(body, status), async () => {
      for (const call of [loadSeries, () => loadEpisodes(1), () => previewImport(preview)]) {
        assert.deepEqual(await call(), { ok: false, error: message });
      }
    });
  }
});

test('non-JSON, absent bodies and network failures return errors', async () => {
  for (const [stub, message] of [
    [async () => new Response('<html>unavailable</html>', { status: 502 }), 'Invalid API response (502).'],
    [async () => new Response('broken', { status: 200 }), 'Invalid API response (200).'],
    [async () => new Response(null, { status: 204 }), 'Empty API response (204).'],
    [async () => { throw new Error('Network unreachable'); }, 'Network unreachable'],
  ]) {
    await withFetch(stub, async () => assert.deepEqual(await loadSeries(), { ok: false, error: message }));
  }
});

test('unsafe response integers are rejected even when nested', async () => {
  for (const body of ['[{"id":9007199254740993}]', '{"target":{"media_type":"episode","id":9007199254740993}}']) {
    await withFetch(async () => new Response(body), async () => {
      const result = await loadSeries();
      assert.equal(result.ok, false);
      assert.match(result.error, /safe range/);
    });
  }
});

test('invalid outgoing IDs never invoke fetch', async () => {
  let calls = 0;
  await withFetch(async () => { calls++; throw new Error('must not fetch'); }, async () => {
    for (const id of [0, -1, 1.5, NaN, Infinity, Number.MAX_SAFE_INTEGER + 1]) {
      assert.equal((await loadEpisodes(id)).ok, false);
      assert.equal((await previewImport({ ...preview, episode_id: id })).ok, false);
    }
  });
  assert.equal(calls, 0);
});

test('response cap counts streamed bytes, cancels oversized body, and releases lock', async () => {
  let cancelled = false;
  let stream;
  await withFetch(async () => {
    stream = new ReadableStream({
      start(controller) {
        controller.enqueue(new Uint8Array(4 * 1024 * 1024));
        controller.enqueue(new Uint8Array(4 * 1024 * 1024 + 1));
      },
      cancel() { cancelled = true; },
    });
    return new Response(stream);
  }, async () => assert.deepEqual(await loadSeries(), { ok: false, error: 'API response exceeds 8 MiB.' }));
  assert.equal(cancelled, true);
  assert.equal(stream.locked, false);
});

test('exactly 8 MiB JSON remains usable across UTF-8 chunk boundaries', async () => {
  const bytes = new TextEncoder().encode('["é"]' + ' '.repeat(8 * 1024 * 1024 - 6));
  assert.equal(bytes.byteLength, 8 * 1024 * 1024);
  await withFetch(async () => new Response(new ReadableStream({
    start(controller) {
      controller.enqueue(bytes.subarray(0, 3)); // Splits the multibyte character.
      controller.enqueue(bytes.subarray(3));
      controller.close();
    },
  })), async () => assert.deepEqual(await loadSeries(), { ok: true, data: ['é'] }));
});

// Native wrappers add status/code for actionable UI recovery; legacy assertions above stay exact.
test('native library calls retain domain, encoded selection, pagination and false/null patches', async () => {
  const calls = [];
  const page = { items: [], total: 0, limit: 25, offset: 25 };
  await withFetch(async (path, options) => {
    calls.push([path, options.method, options.body ? JSON.parse(options.body) : undefined]);
    return json(page);
  }, async () => {
    assert.deepEqual(await listLibrary('tv', 25), { ok: true, data: page });
    await listLibrary('movies');
    await getLibrary('movies', 1);
    await lookupLibrary('tv', 'Show & year:2024');
    await addLibrary('tv', 101, '/tv', { monitored: false });
    await addLibrary('movies', 101, '/movies', { quality_profile_id: null });
    await updateLibrary('tv', 1, { monitored: false });
    await listEpisodes(1, 25);
    await monitorEpisode(1, false);
  });
  assert.deepEqual(calls, [
    ['/api/v1/tv/series?limit=25&offset=25', undefined, undefined],
    ['/api/v1/movies?limit=25&offset=0', undefined, undefined],
    ['/api/v1/movies/1', undefined, undefined],
    ['/api/v1/tv/series/lookup?term=Show+%26+year%3A2024', undefined, undefined],
    ['/api/v1/tv/series/lookup', 'POST', { tvdb_id: 101, path: '/tv', settings: { monitored: false } }],
    ['/api/v1/movies/lookup', 'POST', { tmdb_id: 101, path: '/movies', settings: { quality_profile_id: null } }],
    ['/api/v1/tv/series/1', 'PUT', { monitored: false }],
    ['/api/v1/episodes?series_id=1&limit=25&offset=25', undefined, undefined],
    ['/api/v1/episodes/1', 'PUT', { monitored: false }],
  ]);
});

test('native failures expose static HTTP code/status and preserve nullable success facts', async () => {
  await withFetch(async () => json({ error: { code: 'library_conflict', message: 'Conflict' } }, 409), async () => {
    assert.deepEqual(await addLibrary('movies', 1, '/movies'), { ok: false, error: 'Conflict', code: 'library_conflict', status: 409 });
  });
  const item = { id: 1, media_type: 'movies', year: null, is_available: null, statistics: { size_on_disk: null } };
  await withFetch(async () => json(item), async () => assert.deepEqual(await getLibrary('movies', 1), { ok: true, data: item }));
});

test('typed import execution never retries uncertain mutations and uses readback for recovery', async () => {
  const id = '0d3a11d1-4aba-4dd7-913d-d4b50a9065b3';
  const body = { target: { media_type: 'movie', id: 1 }, source: '/incoming/movie', destination: '/movies/movie', mode: 'copy' };
  const operation = { id, target: body.target, status: 'complete', message: 'Done' };
  const calls = [];
  await withFetch(async (path, options) => {
    calls.push([path, options.method, options.body]);
    if (path.endsWith('/execute')) throw new DOMException('Request timed out', 'TimeoutError');
    return json(operation);
  }, async () => {
    assert.equal((await previewManualImport(body)).ok, true);
    assert.deepEqual(await executeImport(id), { ok: false, error: 'Request timed out' });
    assert.deepEqual(await getImport(id), { ok: true, data: operation });
  });
  assert.deepEqual(calls, [
    ['/api/v1/imports', 'POST', JSON.stringify(body)],
    [`/api/v1/imports/${id}/execute`, 'POST', undefined],
    [`/api/v1/imports/${id}`, undefined, undefined],
  ]);
});

test('native invalid identities fail before fetch including operation path injection', async () => {
  let calls = 0;
  await withFetch(async () => { calls++; throw new Error('unexpected fetch'); }, async () => {
    for (const id of [0, -1, 1.5, Number.MAX_SAFE_INTEGER + 1, NaN]) {
      for (const run of [() => getLibrary('tv', id), () => addLibrary('movies', id, '/m'), () => updateLibrary('tv', id, {}), () => listEpisodes(id), () => monitorEpisode(id, false), () => previewManualImport({target:{media_type:'episode',id},source:'/s',destination:'/d',mode:'copy'})]) {
        assert.equal((await run()).ok, false);
      }
    }
    for (const id of ['', '../movie/1', 'uuid?execute=true']) {
      assert.equal((await getImport(id)).ok, false);
      assert.equal((await executeImport(id)).ok, false);
    }
  });
  assert.equal(calls, 0);
});

test('provider wrappers preserve full scopes and credential omission, replacement and clear', async () => {
  const id = '0d3a11d1-4aba-4dd7-913d-d4b50a9065b3';
  const settings = {implementation:'torznab',endpoint:'http://127.0.0.1:1/torznab',tv:{categories:[5030],anime_categories:[5070],anime_standard_format_search:true},movies:{categories:[2000],remove_year:true}};
  const base = {name:'Indexer',enabled:true,priority:1,settings};
  const calls=[];
  await withFetch(async (path, options) => {
    calls.push([path,options.method,options.body ? JSON.parse(options.body) : undefined]);
    return options.method==='DELETE' ? new Response(null,{status:204}) : json({id,revision:2,has_credentials:true});
  }, async () => {
    await listProviders(25); await getProviderSchema('movies','indexer'); await getProvider(id);
    await createProvider({...base,credentials:{kind:'api_key',api_key:'fixture-good'}});
    await updateProvider(id,{...base,revision:1});
    await updateProvider(id,{...base,revision:2,credentials:{kind:'indexer',api_key:'replacement',tv_parameters:[{name:'private',value:'kept'}],movie_parameters:[]}});
    await updateProvider(id,{...base,revision:3,credentials:null});
    await testProvider(id);
    assert.deepEqual(await deleteProvider(id,4),{ok:true,data:undefined});
  });
  assert.equal(calls[0][0],'/api/v1/providers?limit=25&offset=25');
  assert.equal(calls[1][0],'/api/v1/providers/schema?media_type=movies&kind=indexer');
  assert.deepEqual(calls[3].slice(0,2),['/api/v1/providers','POST']);
  assert.equal(Object.hasOwn(calls[4][2],'credentials'),false);
  assert.deepEqual(calls[4][2].settings,settings);
  assert.deepEqual(calls[5][2].credentials.tv_parameters,[{name:'private',value:'kept'}]);
  assert.equal(calls[6][2].credentials,null);
  assert.deepEqual(calls[7],[`/api/v1/providers/${id}/test`,'POST',undefined]);
  assert.deepEqual(calls[8],[`/api/v1/providers/${id}?revision=4`,'DELETE',undefined]);
});

test('provider revision conflicts remain actionable and uncertain tests do not replay', async () => {
  const id='0d3a11d1-4aba-4dd7-913d-d4b50a9065b3';
  await withFetch(async()=>json({error:{code:'revision_conflict',message:'Configuration changed'}},409),async()=>{
    assert.deepEqual(await testProvider(id),{ok:false,error:'Configuration changed',status:409,code:'revision_conflict'});
  });
  let calls=0;
  await withFetch(async()=>{calls++;throw new DOMException('Timed out','TimeoutError');},async()=>{
    assert.deepEqual(await testProvider(id),{ok:false,error:'Timed out'});
  });
  assert.equal(calls,1);
  await withFetch(async()=>{throw new Error('unexpected fetch');},async()=>{
    assert.equal((await deleteProvider(id,0)).ok,false);
    assert.equal((await updateProvider(id,{revision:Number.MAX_SAFE_INTEGER+1})).ok,false);
    assert.equal((await getProvider('../providers')).ok,false);
    assert.equal((await testProvider('invalid')).ok,false);
  });
});
