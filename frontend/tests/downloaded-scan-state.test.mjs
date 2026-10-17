import test from 'node:test';
import assert from 'node:assert/strict';
import {
  SCAN_PENDING_LIMIT, SCAN_PENDING_BYTES, SCAN_POLL_MS, SCAN_MAX_BACKOFF_MS,
  scanPathProblem, validateScanForm, createPendingScan, encodePendingScans, decodePendingScans, addPendingScan,
  removePendingScan, commandMatches, classifyAdmission, classifyReadback, validScanCommand, pollDelay, shouldPoll,
  canCancelScan, resumeRequest, resumeFailureMessage, completionOwners, describeScan,
} from '../src/lib/downloaded-scan-state.ts';

const id = (n = 1) => `c0000000-0000-4000-8000-${String(n).padStart(12, '0')}`;
const form = {path: '/downloads/Harbor.S01.1080p.WEB-DL', mode: 'auto', priority: 'normal'};
const counts = {entries: 0, ready: 0, importing: 0, imported: 0, already_imported: 0, rejected: 0, failed: 0, cancelled: 0, recovery_required: 0};
const parent = (overrides = {}) => ({
  id: id(), media_type: 'tv', path: form.path, mode: 'auto', priority: 'normal', tracked: null, tracking: 'untracked',
  status: 'queued', phase: 'queued', outcome: null, error_code: null, attempts: 0, run_generation: 0, cancel_requested: false,
  inventory_revision: 0, source: null, next_attempt_at: 1, created_at: 1, started_at: null, completed_at: null, counts: {...counts}, ...overrides,
});
const child = (overrides = {}) => ({
  id: id(9), ordinal: 0, relative_path: 'a.flv', source_path: '/downloads/a.flv', kind: 'file', disposition: 'failed', selected: true,
  reasons: [], series_id: 1, movie_id: null, anchor_episode_id: 1, member_count: 1, expected_old_movie_file_id: null, destination: '/tv/a.flv',
  resolved_mode: 'copy', quality_id: 3, revision: null, edition: null, size: 1, observation: 'measured', error_code: 'import_error',
  operation_id: id(7), operation_phase: 'published', path_failure_code: 'unresolved_attempt', resume_generation: 2, resume_requested: false,
  recovery_required: true, ...overrides,
});
const pending = (n = 1, domain = 'tv') => createPendingScan(domain, id(n), form).data;

test('form validation mirrors the server path rules and never rewrites the path', () => {
  for (const path of ['', '/', 'relative', '/a/', '/a//b', '/a/./b', '/a/../b', '/a\0b', `/${'a'.repeat(4096)}`, '/ä'.repeat(2048), '/bad\uD800']) {
    assert.notEqual(scanPathProblem(path), null, JSON.stringify(path.slice(0, 20)));
    assert.equal(validateScanForm({...form, path}).ok, false);
  }
  for (const path of ['/a', '/downloads/Name with spaces ', '/x/tab\tname', '/x/line\nname', `/${'a'.repeat(4095)}`, '/ä'.repeat(1365)]) {
    assert.deepEqual(validateScanForm({...form, path}), {ok: true, data: {...form, path}});
  }
  assert.equal(validateScanForm({...form, mode: 'hardlink'}).ok, false);
  assert.equal(validateScanForm({...form, priority: 'urgent'}).ok, false);
  assert.equal(createPendingScan('shows', id(), form).ok, false);
  assert.equal(createPendingScan('tv', id().toUpperCase(), form).ok, false, 'Only canonical lowercase IDs are stored.');
  const entry = pending();
  assert.deepEqual(entry, {domain: 'tv', input: {id: id(), ...form}});
  assert.ok(Object.isFrozen(entry) && Object.isFrozen(entry.input), 'Captured requests are immutable.');
});

test('pending identities persist, decode strictly and stay bounded', () => {
  const entries = [pending(1), pending(2, 'movies')];
  const raw = encodePendingScans(entries);
  assert.equal(raw.ok, true);
  assert.deepEqual(decodePendingScans(raw.data), {ok: true, data: entries});
  assert.deepEqual(decodePendingScans(null), {ok: true, data: []});
  for (const bad of ['{', '[]', JSON.stringify({version: 2, entries: []}), JSON.stringify({version: 1, entries: [], extra: 1}),
    JSON.stringify({version: 1, entries: [{...entries[0], state: 'sent'}]}),
    JSON.stringify({version: 1, entries: [{domain: 'tv', input: {...entries[0].input, path: '/a/'}}]}),
    JSON.stringify({version: 1, entries: [{domain: 'tv', input: {...entries[0].input, tracked: null}}]}),
    JSON.stringify({version: 1, entries: [entries[0], entries[0]]}),
    'x'.repeat(SCAN_PENDING_BYTES + 1)]) {
    assert.equal(decodePendingScans(bad).ok, false, bad.slice(0, 60));
  }
  let full = [];
  for (let n = 1; n <= SCAN_PENDING_LIMIT; n++) full = addPendingScan(full, pending(n)).data;
  assert.equal(full.length, SCAN_PENDING_LIMIT);
  assert.match(addPendingScan(full, pending(99)).error, /await confirmation/);
  assert.equal(addPendingScan([pending(1)], pending(1)).ok, false, 'Duplicate identities are refused.');
  assert.equal(encodePendingScans([...full, pending(99)]).ok, false);
  assert.deepEqual(removePendingScan(entries, id(1)), [entries[1]]);
});

test('admission settles only on a matching response or an explicit rejection', () => {
  const entry = pending();
  const admitted = parent();
  assert.equal(commandMatches(entry, admitted), true);
  assert.equal(classifyAdmission(entry, {ok: true, data: admitted}), 'admitted');
  for (const patch of [{id: id(2)}, {media_type: 'movies'}, {path: '/other'}, {mode: 'copy'}, {priority: 'high'}, {tracked: {provider_id: id(3), remote_id: 'a'.repeat(40)}}]) {
    assert.equal(classifyAdmission(entry, {ok: true, data: {...admitted, ...patch}}), 'mismatch', JSON.stringify(patch));
  }
  for (const status of [400, 413, 429]) assert.equal(classifyAdmission(entry, {ok: false, error: 'x', status}), 'rejected');
  assert.equal(classifyAdmission(entry, {ok: false, error: 'x', status: 409, code: 'downloaded_scan_provider_unknown'}), 'rejected');
  assert.equal(classifyAdmission(entry, {ok: false, error: 'x', status: 409, code: 'downloaded_scan_conflict'}), 'mismatch', 'An existing different request keeps this identity.');
  for (const lost of [{ok: false, error: 'timeout'}, {ok: false, error: 'x', status: 503}, {ok: false, error: 'x', status: 500}, {ok: false, error: 'x', status: 404}]) {
    assert.equal(classifyAdmission(entry, lost), 'uncertain', JSON.stringify(lost));
  }
});

test('readback of the same ID distinguishes admitted, current absence and unknown', () => {
  const entry = pending();
  assert.equal(classifyReadback(entry, {ok: true, data: parent({status: 'succeeded', phase: 'settled', outcome: 'success'})}), 'admitted');
  assert.equal(classifyReadback(entry, {ok: true, data: parent({path: '/elsewhere'})}), 'mismatch');
  assert.equal(classifyReadback(entry, {ok: false, error: 'x', status: 404}), 'absent');
  assert.equal(classifyReadback(entry, {ok: false, error: 'x', status: 503}), 'unknown');
  assert.equal(classifyReadback(entry, {ok: false, error: 'network'}), 'unknown');
});

test('parent validation rejects cross-domain and inconsistent rows', () => {
  assert.equal(validScanCommand(parent(), 'tv'), true);
  assert.equal(validScanCommand(parent(), 'movies'), false);
  assert.equal(validScanCommand(parent({status: 'succeeded'}), 'tv'), false, 'Terminal status requires the settled phase.');
  assert.equal(validScanCommand(parent({phase: 'settled'}), 'tv'), false);
  assert.equal(validScanCommand(parent({counts: {...counts, imported: -1}}), 'tv'), false);
  assert.equal(validScanCommand(parent({id: 'x'}), 'tv'), false);
});

test('polling runs only while visible work can change, pauses when hidden and backs off on errors', () => {
  const base = {mounted: true, active: true, hidden: false, unread: false};
  assert.equal(shouldPoll({...base, visible: [{status: 'succeeded'}, {status: 'cancelled'}]}), false);
  for (const status of ['queued', 'running', 'retry_wait']) assert.equal(shouldPoll({...base, visible: [{status: 'failed'}, {status}]}), true, status);
  assert.equal(shouldPoll({...base, visible: [], unread: true}), true, 'An unread or failed list is retried.');
  assert.equal(shouldPoll({...base, hidden: true, visible: [{status: 'running'}]}), false);
  assert.equal(shouldPoll({...base, active: false, visible: [{status: 'running'}]}), false);
  assert.equal(shouldPoll({...base, mounted: false, visible: [{status: 'running'}]}), false);
  assert.equal(pollDelay(0), SCAN_POLL_MS);
  assert.deepEqual([1, 2, 3, 4, 50].map(pollDelay), [10_000, 20_000, 40_000, SCAN_MAX_BACKOFF_MS, SCAN_MAX_BACKOFF_MS]);
  assert.equal(pollDelay(-1), SCAN_POLL_MS);
  assert.equal(pollDelay(NaN), SCAN_POLL_MS);
});

test('resume is offered only when the API reports required recovery and carries the read generation', () => {
  const running = parent({status: 'running', phase: 'recovery_required', inventory_revision: 1});
  assert.deepEqual(resumeRequest(running, child()), {operation_id: id(7), inventory_revision: 1, expected_resume_generation: 2});
  for (const patch of [{recovery_required: false}, {disposition: 'imported'}, {resume_requested: true}, {operation_id: null}]) {
    assert.equal(resumeRequest(running, child(patch)), null, JSON.stringify(patch));
  }
  assert.equal(resumeRequest({...running, status: 'succeeded', phase: 'settled'}, child()), null);
  assert.equal(resumeRequest({...running, inventory_revision: 0}, child()), null);
  assert.match(resumeFailureMessage({ok: false, error: 'x', code: 'downloaded_scan_resume_generation', status: 409}), /changed since it was read/);
  assert.match(resumeFailureMessage({ok: false, error: 'x', code: 'import_busy', status: 409}), /Another import/);
  assert.equal(resumeFailureMessage({ok: false, error: 'raw', status: 409}), 'raw');
  assert.equal(canCancelScan(running), true);
  assert.equal(canCancelScan({...running, cancel_requested: true}), false);
  assert.equal(canCancelScan(parent({status: 'cancelled', phase: 'settled'})), false);
});

test('completion owners are domain-scoped and conservative when the read is incomplete', () => {
  const imported = [child({disposition: 'imported', series_id: 4}), child({id: id(8), disposition: 'imported', series_id: 2}), child({id: id(10), disposition: 'imported', series_id: 4})];
  assert.deepEqual(completionOwners('tv', imported, 3), [2, 4]);
  assert.equal(completionOwners('tv', imported, 101), null);
  assert.equal(completionOwners('movies', imported, 3), null, 'A TV owner never stands in for a movie owner.');
  assert.deepEqual(completionOwners('movies', [child({disposition: 'imported', series_id: null, movie_id: 4})], 1), [4]);
});

test('summaries distinguish success, zero-import, missing path, cancellation and recovery', () => {
  assert.match(describeScan(parent({status: 'succeeded', phase: 'settled', outcome: 'success', counts: {...counts, imported: 1, rejected: 1}})), /^Finished: 1 imported, 1 rejected/);
  assert.match(describeScan(parent({status: 'succeeded', phase: 'settled', outcome: 'unsuccessful', source: 'found'})), /without importing anything/);
  assert.match(describeScan(parent({status: 'succeeded', phase: 'settled', outcome: 'unsuccessful', source: 'missing'})), /does not exist/);
  assert.match(describeScan(parent({status: 'cancelled', phase: 'settled'})), /^Cancelled/);
  assert.match(describeScan(parent({status: 'running', phase: 'recovery_required', counts: {...counts, recovery_required: 1}})), /1 file need explicit recovery/);
  assert.match(describeScan(parent({status: 'running', phase: 'importing', cancel_requested: true})), /Stopping after the current file/);
});
