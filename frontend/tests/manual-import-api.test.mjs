import test from 'node:test';
import assert from 'node:assert/strict';
import { createManualImportCommands, getManualImportCommand, cancelManualImportCommand, reconcileManualImports } from '../src/lib/api.ts';

const operation = 'a0000000-0000-4000-8000-000000000001';
const second = 'a0000000-0000-4000-8000-000000000002';
const command = 'b0000000-0000-4000-8000-000000000001';
async function withFetch(fetch, run) {
  const original = globalThis.fetch;
  globalThis.fetch = fetch;
  try { await run(); } finally { globalThis.fetch = original; }
}
const response = (data, status = 200) => new Response(JSON.stringify(data), {status});

test('manual import requests retain UUIDs, priority and command-specific cancellation', async () => {
  const calls = [];
  await withFetch(async (path, options) => {
    assert.ok(options.signal instanceof AbortSignal);
    calls.push({path, method: options.method, body: options.body && JSON.parse(options.body)});
    return response([]);
  }, async () => {
    assert.equal((await createManualImportCommands({operation_ids: [operation, second], priority: 'high'})).ok, true);
    assert.equal((await getManualImportCommand(command)).ok, true);
    assert.equal((await cancelManualImportCommand(command)).ok, true);
    assert.equal((await reconcileManualImports([second, operation])).ok, true);
  });
  assert.deepEqual(calls, [
    {path: '/api/v1/manual-import/commands', method: 'POST', body: {operation_ids: [operation, second], priority: 'high'}},
    {path: `/api/v1/manual-import/commands/${command}`, method: undefined, body: undefined},
    {path: `/api/v1/manual-import/commands/${command}/cancel`, method: 'POST', body: undefined},
    {path: '/api/v1/manual-import/commands/reconcile', method: 'POST', body: {operation_ids: [second, operation]}},
  ]);
});

test('invalid manual import identities and over-cap batches never reach the network', async () => {
  await withFetch(() => assert.fail('Invalid input reached fetch'), async () => {
    for (const ids of [[], [operation, operation.toUpperCase()], ['../../execute'], ['not-a-uuid'], [null], Array(101).fill(operation)]) {
      assert.equal((await createManualImportCommands({operation_ids: ids, priority: 'normal'})).ok, false);
      assert.equal((await reconcileManualImports(ids)).ok, false);
    }
    assert.equal((await createManualImportCommands({operation_ids: [operation], priority: 'urgent'})).ok, false);
    for (const id of ['../cancel', '', 'not-a-uuid']) {
      assert.equal((await getManualImportCommand(id)).ok, false);
      assert.equal((await cancelManualImportCommand(id)).ok, false);
    }
  });
});

test('lost admission and cancel responses are returned without automatic resubmission', async () => {
  for (const invoke of [() => createManualImportCommands({operation_ids: [operation], priority: 'normal'}), () => cancelManualImportCommand(command)]) {
    let calls = 0;
    await withFetch(async () => { calls++; throw new Error('Response lost'); }, async () => {
      assert.deepEqual(await invoke(), {ok: false, error: 'Response lost'});
    });
    assert.equal(calls, 1);
  }
});

test('read-only reconciliation preserves explicit missing rows and native conflict details', async () => {
  const rows = [{operation_id: operation, operation: null, active_command: null}];
  await withFetch(async () => response(rows), async () => {
    assert.deepEqual(await reconcileManualImports([operation]), {ok: true, data: rows});
  });
  await withFetch(async () => response({error: {code: 'command_conflict', message: 'Command is already active'}}, 409), async () => {
    assert.deepEqual(await createManualImportCommands({operation_ids: [operation], priority: 'normal'}), {
      ok: false, status: 409, code: 'command_conflict', error: 'Command is already active',
    });
  });
});
