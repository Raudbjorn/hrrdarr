import test from 'node:test';
import assert from 'node:assert/strict';
import {
  SCAN_PENDING_LIMIT, SCAN_PENDING_BYTES, SCAN_POLL_MS, SCAN_MAX_BACKOFF_MS,
  scanPathProblem, validateScanForm, createPendingScan, encodePendingScans, decodePendingScans, addPendingScan,
  removePendingScan, commandMatches, classifyAdmission, classifyReadback, validScanCommand, pollDelay, shouldPoll,
  canCancelScan, resumeRequest, resumeFailureMessage, completionOwners, describeScan,
  describeCleanup, describeCoverage, cleanupResumeRequest, cleanupResumeFailureMessage,
  trackedProblem, successorRequest, successorConfirmation, admissionFailureMessage, trackingProviders,
} from '../src/lib/downloaded-scan-state.ts';

const id = (n = 1) => `c0000000-0000-4000-8000-${String(n).padStart(12, '0')}`;
const form = {path: '/downloads/Harbor.S01.1080p.WEB-DL', mode: 'auto', priority: 'normal'};
const counts = {entries: 0, ready: 0, importing: 0, imported: 0, already_imported: 0, rejected: 0, failed: 0, cancelled: 0, recovery_required: 0};
const scanCoverage = {state: 'untracked', expected: 0, covered: 0, missing: 0};
const scanCleanup = {state: 'not_needed', phase: null, reason: null, removed: 0, error_code: null, resume_generation: 0, resume_requested: false, recovery_required: false};
const parent = (overrides = {}) => ({
  id: id(), media_type: 'tv', path: form.path, mode: 'auto', priority: 'normal', tracked: null, tracking: 'untracked',
  status: 'queued', phase: 'queued', outcome: null, error_code: null, attempts: 0, run_generation: 0, cancel_requested: false,
  inventory_revision: 0, source: null, next_attempt_at: 1, created_at: 1, started_at: null, completed_at: null, counts: {...counts},
  receipt_id: null, predecessor_operation_id: null, coverage: {...scanCoverage}, cleanup: {...scanCleanup},
  supersedes_command_id: null, successor_command_id: null, successor_allowed: false, ...overrides,
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
    // Reasoning: the validated form now carries its (absent) tracked identity explicitly.
    assert.deepEqual(validateScanForm({...form, path}), {ok: true, data: {...form, path, tracked: null}});
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
  // The panel renders coverage and cleanup; a row without them is not a usable parent.
  assert.equal(validScanCommand(parent({coverage: undefined}), 'tv'), false);
  assert.equal(validScanCommand(parent({coverage: {...scanCoverage, missing: 'two'}}), 'tv'), false);
  assert.equal(validScanCommand(parent({cleanup: undefined}), 'tv'), false);
  assert.equal(validScanCommand(parent({cleanup: {...scanCleanup, recovery_required: 'yes'}}), 'tv'), false);
  assert.equal(validScanCommand(parent({cleanup: {...scanCleanup, phase: 'removed'}}), 'tv'), false);
  assert.equal(validScanCommand(parent({tracking: 'tracked'}), 'tv'), false);
  assert.equal(validScanCommand(parent({source: 'other'}), 'tv'), false);
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

const cleanup = (overrides = {}) => ({
  state: 'not_needed', phase: null, reason: null, removed: 0, error_code: null, resume_generation: 0, resume_requested: false,
  recovery_required: false, ...overrides,
});

test('cleanup and coverage summaries use static reasons and never imply recovery that is not required', () => {
  assert.match(describeCleanup(cleanup()), /^Not needed/);
  assert.match(describeCleanup(cleanup({state: 'complete', phase: 'settled', removed: 3})), /3 emptied folders/);
  assert.match(describeCleanup(cleanup({state: 'retained', phase: 'settled', reason: 'unimported_media', removed: 1})), /not imported remain .*1 emptied folder\)/);
  assert.match(describeCleanup(cleanup({state: 'retained', phase: 'settled', reason: 'something_new'})), /leftovers remain/);
  assert.match(describeCleanup(cleanup({state: 'pending', phase: 'observed'})), /^Checking/);
  assert.match(describeCleanup(cleanup({state: 'pending', phase: 'remove_intent', recovery_required: true, error_code: 'cleanup_io'})), /interrupted \(cleanup_io\)/);
  assert.equal(describeCoverage({state: 'untracked', expected: 0, covered: 0, missing: 0}), null);
  assert.match(describeCoverage({state: 'incomplete', expected: 1, covered: 0, missing: 1}), /stays blocked/);
  assert.match(describeCoverage({state: 'complete', expected: 1, covered: 1, missing: 0}), /^Complete: 1 of 1/);
  const stuck = parent({status: 'running', phase: 'recovery_required', inventory_revision: 1, cleanup: cleanup({state: 'pending', phase: 'remove_intent', recovery_required: true, resume_generation: 2})});
  assert.match(describeScan(stuck), /cleanup was interrupted/);
});

test('cleanup recovery body names the exact generation and is offered only when required', () => {
  const stuck = parent({status: 'running', phase: 'recovery_required', inventory_revision: 1, cleanup: cleanup({state: 'pending', phase: 'remove_intent', recovery_required: true, resume_generation: 2})});
  assert.deepEqual(cleanupResumeRequest(stuck), {inventory_revision: 1, expected_cleanup_resume_generation: 2});
  assert.equal(cleanupResumeRequest({...stuck, cleanup: cleanup({state: 'pending', phase: 'remove_intent', resume_requested: true})}), null);
  assert.equal(cleanupResumeRequest({...stuck, status: 'succeeded', phase: 'settled'}), null);
  assert.equal(cleanupResumeRequest({...stuck, cleanup: {...stuck.cleanup, resume_generation: -1}}), null);
  assert.match(cleanupResumeFailureMessage({ok: false, status: 409, code: 'downloaded_scan_cleanup_resume_generation', error: 'x'}), /changed since it was read/);
  assert.equal(cleanupResumeFailureMessage({ok: false, status: 500, code: 'other', error: 'raw'}), 'raw');
});

const provider = id(41), hash = 'a'.repeat(40);
const tracked = {...form, mode: 'copy', providerId: provider, remoteId: hash};

test('tracked identity is optional, exact and mirrors the server grammar', () => {
  assert.equal(trackedProblem('', ''), null);
  for (const [p, r] of [[provider, ''], ['', hash], ['not-a-uuid', hash], [provider, 'A'.repeat(40)], [provider, 'a'.repeat(39)], [provider, 'g'.repeat(40)], [provider, 'a'.repeat(41)]])
    assert.notEqual(trackedProblem(p, r), null, `${p} ${r}`);
  assert.equal(trackedProblem(provider, 'b'.repeat(64)), null);
  assert.deepEqual(validateScanForm(tracked), {ok: true, data: {path: form.path, mode: 'copy', priority: 'normal', tracked: {provider_id: provider, remote_id: hash}}});
  const entry = createPendingScan('movies', id(2), tracked);
  assert.equal(entry.ok, true);
  assert.deepEqual(entry.data.input, {id: id(2), path: form.path, mode: 'copy', priority: 'normal', tracked: {provider_id: provider, remote_id: hash}});
  // Untracked requests carry no tracked key at all (the server treats absence as untracked).
  assert.deepEqual(Object.keys(pending().input).sort(), ['id', 'mode', 'path', 'priority']);
  // Retained identities round-trip with their tracked and successor fields, strictly.
  const successor = createPendingScan('tv', id(3), tracked, id(4)).data;
  const decoded = decodePendingScans(encodePendingScans([entry.data, successor]).data);
  assert.equal(decoded.ok, true);
  assert.deepEqual(decoded.data.map(e => e.input), [entry.data.input, successor.input]);
  for (const bad of [
    {version: 1, entries: [{domain: 'tv', input: {...successor.input, extra: 1}}]},
    {version: 1, entries: [{domain: 'tv', input: {...successor.input, tracked: {provider_id: provider}}}]},
    {version: 1, entries: [{domain: 'tv', input: {...entry.data.input, supersedes_command_id: id(4), tracked: undefined}}]},
    {version: 1, entries: [{domain: 'tv', input: {...successor.input, supersedes_command_id: 7}}]},
  ]) assert.equal(decodePendingScans(JSON.stringify(bad)).ok, false, JSON.stringify(bad));
});

test('a server row matches a retained request only with the same tracked identity and lineage', () => {
  const entry = createPendingScan('tv', id(5), tracked, id(6)).data;
  const row = parent({id: id(5), mode: 'copy', tracked: {provider_id: provider, remote_id: hash}, supersedes_command_id: id(6)});
  assert.equal(commandMatches(entry, row), true);
  assert.equal(commandMatches(entry, {...row, supersedes_command_id: null}), false);
  assert.equal(commandMatches(entry, {...row, tracked: null}), false);
  assert.equal(commandMatches(entry, {...row, tracked: {provider_id: provider, remote_id: 'b'.repeat(40)}}), false);
  assert.equal(commandMatches(pending(), parent({tracked: {provider_id: provider, remote_id: hash}})), false);
  assert.equal(classifyAdmission(entry, {ok: true, data: row}), 'admitted');
  // Successor lineage refusals happen inside the admission transaction: nothing was created.
  for (const code of ['downloaded_scan_successor_invalid', 'downloaded_scan_successor_not_settled', 'downloaded_scan_successor_not_incomplete',
    'downloaded_scan_successor_conflict', 'downloaded_scan_successor_receipt_unavailable'])
    assert.equal(classifyAdmission(entry, {ok: false, status: 409, code, error: 'x'}), 'rejected', code);
  // The same id naming a different request keeps its identity; an unknown failure is uncertain.
  assert.equal(classifyAdmission(entry, {ok: false, status: 409, code: 'downloaded_scan_conflict', error: 'x'}), 'mismatch');
  assert.equal(classifyAdmission(entry, {ok: false, status: 503, error: 'x'}), 'uncertain');
  assert.match(admissionFailureMessage({ok: false, code: 'downloaded_scan_successor_conflict', error: 'raw'}), /Another scan already continues/);
  assert.equal(admissionFailureMessage({ok: false, code: 'other', error: 'raw'}), 'raw');
  assert.equal(validScanCommand({...row, successor_allowed: 'yes'}, 'tv'), false);
  assert.equal(validScanCommand({...row, supersedes_command_id: 'x'}, 'tv'), false);
});

test('"Scan remaining files" is a new reviewed request naming the settled incomplete scan', () => {
  const settled = parent({id: id(10), status: 'succeeded', phase: 'settled', outcome: 'unsuccessful', mode: 'move', priority: 'high',
    tracked: {provider_id: provider, remote_id: hash}, tracking: 'receipt', inventory_revision: 1,
    coverage: {state: 'incomplete', expected: 1, covered: 0, missing: 1}, successor_allowed: true});
  const request = successorRequest(settled, id(11));
  assert.equal(request.ok, true);
  assert.deepEqual(request.data, {domain: 'tv', input: {id: id(11), path: form.path, mode: 'move', priority: 'high',
    tracked: {provider_id: provider, remote_id: hash}, supersedes_command_id: id(10)}});
  assert.ok(Object.isFrozen(request.data.input));
  // Never reopens the original: the identity must be new.
  assert.equal(successorRequest(settled, id(10)).ok, false);
  for (const blocked of [{successor_allowed: false}, {status: 'running', phase: 'awaiting_import'}, {coverage: {state: 'complete', expected: 1, covered: 1, missing: 0}}, {tracked: null}])
    assert.equal(successorRequest({...settled, ...blocked}, id(12)), null, JSON.stringify(blocked));
  assert.match(successorConfirmation(settled), /1 of 1/);
  assert.match(successorConfirmation(settled), /finished scan stays unchanged/);
});

test('tracking choices are download clients configured for the library type only', () => {
  const settings = (implementation, tv, movies) => ({implementation, endpoint: 'http://x', tv, movies});
  const providers = [
    {id: id(20), name: 'Indexer', settings: settings('torznab', {}, {})},
    // The expanded registry includes RSS indexers; only actual download clients may track scans.
    {id: id(23), name: 'RSS indexer', settings: settings('torrentrss', {}, {})},
    {id: id(24), name: 'NZBGet', settings: settings('nzbget', null, {category: 'movies'})},
    {id: id(21), name: 'TV client', settings: settings('qbittorrent', {category: 'tv'}, null)},
    {id: id(22), name: 'Both', settings: settings('qbittorrent', {category: 'tv'}, {category: 'movies'})},
    {id: 'bad', name: 'Malformed', settings: settings('qbittorrent', {}, {})},
  ];
  assert.deepEqual(trackingProviders(providers, 'tv').map(p => p.name), ['TV client', 'Both']);
  assert.deepEqual(trackingProviders(providers, 'movies').map(p => p.name), ['NZBGet', 'Both']);
});
