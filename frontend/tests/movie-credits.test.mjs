import test from 'node:test';
import assert from 'node:assert/strict';
import { listMovieCredits } from '../src/lib/api.ts';

test('movie credits requests the full bounded movie collection and preserves namesakes and multiple roles', async () => {
  const original = globalThis.fetch, calls = [];
  const items = Array.from({length: 125}, (_, index) => ({id: index + 1, metadata_id: 9, credit_tmdb_id: `credit-${index}`, person_tmdb_id: index < 2 ? 7 : index + 7, person_name: 'Shared name', type: index < 110 ? 'cast' : 'crew', order: index, character: `Role ${index}`, job: null, department: null, images: []}));
  const page = {items, total: items.length, limit: 500, offset: 0};
  globalThis.fetch = async (path, options) => { calls.push([path, options.method, options.body]); return Response.json(page); };
  try {
    assert.deepEqual(await listMovieCredits(3), {ok: true, data: page});
    assert.deepEqual(calls, [['/api/v1/movies/credits?movie_id=3&limit=500', undefined, undefined]]);
  } finally { globalThis.fetch = original; }
});

test('movie credits rejects unsafe movie IDs before fetching', async () => {
  const original = globalThis.fetch;
  let calls = 0;
  globalThis.fetch = async () => { calls++; return Response.json({}); };
  try {
    for (const id of [0, -1, 1.5, NaN, Infinity, 9007199254740992, '1']) assert.equal((await listMovieCredits(id)).ok, false);
    assert.equal(calls, 0);
    await listMovieCredits(Number.MAX_SAFE_INTEGER);
    assert.equal(calls, 1);
  } finally { globalThis.fetch = original; }
});

test('movie credits preserves native errors and rejects invalid transport responses', async () => {
  const original = globalThis.fetch;
  try {
    for (const [status, code] of [[404, 'credit_not_found'], [500, 'database_error']]) {
      globalThis.fetch = async () => Response.json({error: {code, message: 'Credits unavailable'}}, {status});
      assert.deepEqual(await listMovieCredits(1), {ok: false, status, code, error: 'Credits unavailable'});
    }
    for (const response of [() => new Response('not json'), () => Response.json({person_tmdb_id: 9007199254740992})]) {
      globalThis.fetch = async () => response();
      assert.equal((await listMovieCredits(1)).ok, false);
    }
    globalThis.fetch = async () => { throw new TypeError('owned disconnect'); };
    assert.equal((await listMovieCredits(1)).ok, false);
  } finally { globalThis.fetch = original; }
});
