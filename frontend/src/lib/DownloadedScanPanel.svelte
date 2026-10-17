<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import type { ApiPage, CommandPriority, DownloadedScanCommand, MediaDomain, ScanChild, ScanChildResumeInput, ScanCleanupResumeInput, ScanDisposition, ScanMode } from './api.generated';
  import type { SettingsWriteState } from './api';
  import { cancelDownloadedScanCommand, createDownloadedScanCommand, getDownloadedScanCommand, listDownloadedScanChildren, listDownloadedScanCommands, resumeDownloadedScanChild, resumeDownloadedScanCleanup } from './api';
  import {
    SCAN_CHILD_PAGE, SCAN_OWNER_READ_LIMIT, SCAN_PARENT_PAGE, SCAN_PENDING_KEY, SCAN_PENDING_LIMIT,
    addPendingScan, canCancelScan, classifyAdmission, cleanupResumeFailureMessage, cleanupResumeRequest, describeCleanup, describeCoverage, classifyReadback, completionOwners, createPendingScan,
    decodePendingScans, describeScan, encodePendingScans, isTerminalScan, pollDelay, removePendingScan,
    resumeFailureMessage, resumeRequest, scanPhaseText, shouldPoll, validScanChild, validScanCommand, validateScanForm,
  } from './downloaded-scan-state';
  import type { PendingScan } from './downloaded-scan-state';

  let { domain, active = true, relatedWrite = 'idle', onwrite, oncomplete }: {
    domain: MediaDomain; active?: boolean; relatedWrite?: SettingsWriteState;
    onwrite?: (state: SettingsWriteState) => void;
    oncomplete?: (event: { domain: MediaDomain; commandId: string; owners: number[] | null }) => void;
  } = $props();

  type PendingView = { state: 'checking' | 'absent' | 'unknown' | 'mismatch'; message: string };
  const DISPOSITIONS: ScanDisposition[] = ['ready', 'importing', 'imported', 'already_imported', 'rejected', 'failed', 'cancelled'];
  const label = (scope: MediaDomain) => scope === 'tv' ? 'TV' : 'Movies';

  // The form keeps its own captured domain; a library type change never silently retargets it.
  let scope = $state<MediaDomain>(untrack(() => domain)), keptDomain = $state<MediaDomain | null>(null);
  let path = $state(''), mode = $state<ScanMode>('copy'), priority = $state<CommandPriority>('normal');
  let pending = $state<PendingScan[]>([]), pendingViews = $state<Record<string, PendingView>>({});
  let page = $state<ApiPage<DownloadedScanCommand> | null>(null), parentOffset = $state(0);
  let selectedId = $state<string | null>(null), selected = $state<DownloadedScanCommand | null>(null);
  let children = $state<ApiPage<ScanChild> | null>(null), childOffset = $state(0), childFilter = $state<ScanDisposition | ''>('');
  let lostResumes = $state<Record<string, ScanChildResumeInput>>({});
  // A lost cleanup-recovery response, kept in memory only: repeating the identical body is safe.
  let lostCleanup = $state<{ id: string; body: ScanCleanupResumeInput } | null>(null);
  let busy = $state(false), checking = $state(false), ready = $state(false), fresh = $state(false), unread = $state(true);
  let error = $state(''), notice = $state(''), readError = $state(''), storageError = $state(''), storageBlocked = $state(false);
  let mounted = false, generation = 0, failures = 0, rerun = false, lastStored: string | null = null;
  let timer: ReturnType<typeof setTimeout> | undefined;
  // Parents seen non-terminal (or admitted here) notify completion once per page session.
  const watched = new Set<string>(), completed = new Set<string>();

  const formDirty = $derived(path !== '' || mode !== 'copy' || priority !== 'normal');
  const domainChanged = $derived(domain !== scope && keptDomain !== domain);
  const blocked = $derived(busy || !ready || relatedWrite !== 'idle' || storageBlocked || !!storageError);
  const liveStatus = $derived(selected ? `${selected.path}: ${describeScan(selected)}` : '');

  $effect(() => { onwrite?.(busy ? 'busy' : 'idle'); });
  // Follow the library type only while the form is untouched and no request is in flight.
  $effect(() => {
    const next = domain;
    untrack(() => { if (next !== scope && !formDirty && !busy) switchDomain(next); });
  });
  $effect(() => {
    if (!ready) return;
    const visible = active;
    untrack(() => { invalidate(); if (visible) void refresh(); else clearTimeout(timer); });
  });

  function switchDomain(next: MediaDomain) {
    scope = next; keptDomain = null; page = null; parentOffset = 0; selectedId = null; selected = null;
    children = null; childOffset = 0; childFilter = ''; lostResumes = {}; lostCleanup = null; unread = true; readError = ''; error = ''; notice = '';
    invalidate(); if (mounted) void refresh();
  }
  function discardAndSwitch() {
    if (busy) return;
    if (formDirty && !window.confirm('Discard this scan form and use the selected library type?')) return;
    path = ''; mode = 'copy'; priority = 'normal'; switchDomain(domain);
  }
  function invalidate() { ++generation; fresh = false; }
  function save(next: PendingScan[], keepOnFailure = true): boolean {
    const encoded = encodePendingScans(next);
    if (!encoded.ok) { storageError = encoded.error; if (keepOnFailure) pending = next; return false; }
    if (storageBlocked) { if (keepOnFailure) pending = next; return false; }
    try {
      if (localStorage.getItem(SCAN_PENDING_KEY) !== lastStored) {
        storageBlocked = true; storageError = 'Saved scan requests changed in another tab. New scans are blocked here; reload this page to reconcile them.';
        if (keepOnFailure) pending = next; return false;
      }
      if (next.length) localStorage.setItem(SCAN_PENDING_KEY, encoded.data); else localStorage.removeItem(SCAN_PENDING_KEY);
      lastStored = next.length ? encoded.data : null; pending = next; storageError = ''; return true;
    } catch {
      storageError = 'Browser storage is unavailable or full. New scans are blocked because a lost response could not be reconciled. Keep this tab open.';
      if (keepOnFailure) pending = next; return false;
    }
  }
  function schedule(delay = pollDelay(failures)) {
    clearTimeout(timer);
    const visible = [...(page?.items ?? []), ...(selected ? [selected] : [])];
    if (!busy && !checking && shouldPoll({ mounted, active, hidden: document.hidden, visible, unread }))
      timer = setTimeout(() => { void refresh(); }, delay);
  }
  function track(parent: DownloadedScanCommand) {
    if (!isTerminalScan(parent.status)) { watched.add(parent.id); return; }
    if (!watched.has(parent.id) || completed.has(parent.id)) return;
    completed.add(parent.id);
    if (parent.counts.imported > 0) void notifyCompletion(parent);
  }
  async function notifyCompletion(parent: DownloadedScanCommand) {
    const owner = parent.media_type;
    const imported = await listDownloadedScanChildren(owner, parent.id, { disposition: 'imported', limit: SCAN_OWNER_READ_LIMIT });
    if (!mounted) return;
    const items = imported.ok ? imported.data.items.filter(validScanChild) : [];
    // An unreadable owner set still refreshes conservatively; it never invents an owner.
    const owners = imported.ok && items.length === imported.data.items.length ? completionOwners(owner, items, imported.data.total) : null;
    oncomplete?.({ domain: owner, commandId: parent.id, owners });
  }
  /** One request cycle at a time: parent page, selected parent and its current child page. */
  async function refresh() {
    if (!mounted) return;
    if (checking) { rerun = true; return; }
    clearTimeout(timer); checking = true; rerun = false;
    const ticket = ++generation, owner = scope, offset = parentOffset, id = selectedId, coffset = childOffset, filter = childFilter;
    const [list, detail, rows] = await Promise.all([
      listDownloadedScanCommands(owner, { limit: SCAN_PARENT_PAGE, offset }),
      id ? getDownloadedScanCommand(owner, id) : null,
      id ? listDownloadedScanChildren(owner, id, { limit: SCAN_CHILD_PAGE, offset: coffset, ...(filter ? { disposition: filter } : {}) }) : null,
    ]);
    checking = false;
    if (!mounted) return;
    if (ticket !== generation || owner !== scope) { if (rerun || owner !== scope) void refresh(); else schedule(); return; }
    const problems: string[] = [];
    if (!list.ok) problems.push(list.error);
    else if (!list.data.items.every(item => validScanCommand(item, owner))) problems.push('The scan list response does not match this library type.');
    else { page = list.data; for (const item of list.data.items) track(item); }
    if (detail) {
      if (!detail.ok) problems.push(detail.status === 404 ? 'The selected scan no longer exists for this library type.' : detail.error);
      else if (!validScanCommand(detail.data, owner) || detail.data.id !== id) problems.push('The scan detail response does not match the selected scan.');
      else { selected = detail.data; track(detail.data); }
    }
    if (rows) {
      if (!rows.ok) problems.push(rows.error);
      else if (!rows.data.items.every(validScanChild)) problems.push('The scan entries response is invalid.');
      else children = rows.data;
    }
    readError = problems.join(' ');
    fresh = !problems.length; unread = !page || !!problems.length;
    failures = problems.length ? Math.min(failures + 1, 4) : 0;
    if (rerun) void refresh(); else schedule();
  }
  async function select(id: string) {
    if (selectedId === id) return;
    selectedId = id; selected = page?.items.find(item => item.id === id) ?? null; children = null; childOffset = 0; childFilter = ''; lostResumes = {}; lostCleanup = null;
    invalidate(); await refresh();
  }
  async function admit(entry: PendingScan, replay: boolean) {
    busy = true; invalidate(); error = ''; notice = '';
    pendingViews = { ...pendingViews, [entry.input.id]: { state: 'checking', message: '' } };
    const response = await createDownloadedScanCommand(entry.domain, { ...entry.input });
    if (!mounted) return;
    const outcome = classifyAdmission(entry, response);
    if (outcome === 'admitted' && response.ok) {
      save(removePendingScan(pending, entry.input.id));
      const { [entry.input.id]: _, ...rest } = pendingViews; pendingViews = rest;
      watched.add(response.data.id); track(response.data);
      if (entry.domain === scope) { selectedId = response.data.id; selected = response.data; children = null; childOffset = 0; childFilter = ''; parentOffset = 0; }
      notice = replay ? 'The same scan request was confirmed by the server.' : 'Scan admitted. Discovery and import run in the background; progress updates below.';
      if (!replay && entry.domain === scope && path === entry.input.path) path = '';
    } else if (outcome === 'rejected') {
      save(removePendingScan(pending, entry.input.id));
      const { [entry.input.id]: _, ...rest } = pendingViews; pendingViews = rest;
      error = `${response.ok ? 'Unexpected response.' : response.error} The scan was rejected; nothing was scanned or imported.`;
    } else {
      const message = outcome === 'mismatch'
        ? 'The server returned a different scan for this request ID. Keep this identity and review the scan list before starting another scan.'
        : `${response.ok ? '' : response.error} Admission outcome unknown. Check outcome reads this same request ID; it never creates a new one.`;
      pendingViews = { ...pendingViews, [entry.input.id]: { state: outcome === 'mismatch' ? 'mismatch' : 'unknown', message } };
      error = message;
    }
    busy = false; await refresh();
  }
  async function submit() {
    if (blocked) return;
    const form = validateScanForm({ path, mode, priority });
    if (!form.ok) { error = form.error; return; }
    const entry = createPendingScan(scope, crypto.randomUUID(), form.data);
    if (!entry.ok) { error = entry.error; return; }
    const next = addPendingScan(pending, entry.data);
    if (!next.ok) { error = next.error; return; }
    // Persist the identity BEFORE the POST so a lost response is reconciled by the same ID.
    if (!save(next.data, false)) { error = storageError || 'The scan request could not be saved. Nothing was sent.'; return; }
    await admit(entry.data, false);
  }
  async function checkPending(entry: PendingScan) {
    pendingViews = { ...pendingViews, [entry.input.id]: { state: 'checking', message: '' } };
    const result = await getDownloadedScanCommand(entry.domain, entry.input.id);
    if (!mounted) return;
    const outcome = classifyReadback(entry, result);
    if (outcome === 'admitted' && result.ok) {
      save(removePendingScan(pending, entry.input.id));
      const { [entry.input.id]: _, ...rest } = pendingViews; pendingViews = rest;
      watched.add(result.data.id); track(result.data);
      notice = 'A retained scan request was found on the server.';
      if (entry.domain === scope) { selectedId = result.data.id; selected = result.data; children = null; childOffset = 0; childFilter = ''; invalidate(); void refresh(); }
      return;
    }
    pendingViews = { ...pendingViews, [entry.input.id]: outcome === 'absent'
      ? { state: 'absent', message: 'Not found on the server right now. A delayed request could still arrive; sending the same request again is safe.' }
      : outcome === 'mismatch'
        ? { state: 'mismatch', message: 'This request ID names a different scan on the server. Keep this identity and review the scan list.' }
        : { state: 'unknown', message: `${result.ok ? '' : result.error} Outcome still unknown.` } };
  }
  function discardPending(entry: PendingScan) {
    if (busy || pendingViews[entry.input.id]?.state !== 'absent') return;
    if (!window.confirm('Forget this unconfirmed scan request? If the original request is still delayed it could still run, and it will then appear in the scan list.')) return;
    if (save(removePendingScan(pending, entry.input.id), false)) {
      const { [entry.input.id]: _, ...rest } = pendingViews; pendingViews = rest;
      notice = 'Unconfirmed scan request forgotten in this browser.';
    }
  }
  async function cancel() {
    const parent = selected;
    if (blocked || !fresh || !parent || isTerminalScan(parent.status)) return;
    busy = true; invalidate(); error = ''; notice = '';
    const response = await cancelDownloadedScanCommand(parent.media_type, parent.id);
    if (!mounted) return;
    if (response.ok && validScanCommand(response.data, parent.media_type) && response.data.id === parent.id) {
      if (selectedId === parent.id) selected = response.data;
      track(response.data);
      notice = response.data.status === 'cancelled' ? 'Scan cancelled. Files already imported stay imported.'
        : isTerminalScan(response.data.status) ? `The scan had already finished (${response.data.status}).`
        : 'Cancellation recorded. A file that is already importing finishes first; no further files start.';
    } else error = `${response.ok ? 'Unexpected cancel response.' : response.error} Cancel outcome unknown. Cancelling is idempotent: refresh, then request it again if the scan is still running.`;
    busy = false; await refresh();
  }
  async function resume(parent: DownloadedScanCommand, child: ScanChild, repeat?: ScanChildResumeInput) {
    const body = repeat ?? resumeRequest(parent, child);
    if (blocked || !body || (!repeat && !fresh)) return;
    busy = true; invalidate(); error = ''; notice = '';
    const response = await resumeDownloadedScanChild(parent.media_type, parent.id, child.id, body);
    if (!mounted) return;
    if (response.ok) {
      const { [child.id]: _, ...rest } = lostResumes; lostResumes = rest;
      notice = `Recovery requested for ${child.relative_path}. The worker continues this same import operation.`;
    } else if (response.status === 409 || response.status === 404 || response.status === 400) {
      const { [child.id]: _, ...rest } = lostResumes; lostResumes = rest;
      error = resumeFailureMessage(response);
    } else {
      lostResumes = { ...lostResumes, [child.id]: body };
      error = `${response.error} Recovery outcome unknown. Repeating the identical request is safe; refresh to read the file's state.`;
    }
    busy = false; await refresh();
  }
  async function resumeCleanup(parent: DownloadedScanCommand, repeat?: ScanCleanupResumeInput) {
    const body = repeat ?? cleanupResumeRequest(parent);
    if (blocked || !body || (!repeat && !fresh)) return;
    busy = true; invalidate(); error = ''; notice = '';
    const response = await resumeDownloadedScanCleanup(parent.media_type, parent.id, body);
    if (!mounted) return;
    if (response.ok) {
      lostCleanup = null;
      notice = 'Cleanup recovery requested. The worker re-checks the folder and removes only emptied folders.';
    } else if (response.status === 409 || response.status === 404 || response.status === 400) {
      lostCleanup = null;
      error = cleanupResumeFailureMessage(response);
    } else {
      lostCleanup = { id: parent.id, body };
      error = `${response.error} Cleanup recovery outcome unknown. Repeating the identical request is safe; refresh to read its state.`;
    }
    busy = false; await refresh();
  }
  function changeParentPage(offset: number) { parentOffset = Math.max(0, offset); invalidate(); void refresh(); }
  function changeChildPage(offset: number) { childOffset = Math.max(0, offset); invalidate(); void refresh(); }
  function changeFilter(value: ScanDisposition | '') { childFilter = value; childOffset = 0; invalidate(); void refresh(); }
  const when = (seconds: number | null) => seconds === null ? '—' : new Date(seconds * 1000).toLocaleString();

  onMount(() => {
    mounted = true;
    try {
      lastStored = localStorage.getItem(SCAN_PENDING_KEY);
      const decoded = decodePendingScans(lastStored);
      if (decoded.ok) pending = decoded.data;
      else { storageBlocked = true; storageError = `${decoded.error} New scans are blocked until the saved requests are recovered.`; }
    } catch { storageBlocked = true; storageError = 'Saved scan requests could not be read. New scans are blocked.'; }
    ready = true;
    // Reads only: restored identities are checked by GET of the same ID, never re-sent.
    void (async () => { for (const entry of [...pending]) { if (!mounted) return; await checkPending(entry); } })();
    const visibility = () => { if (document.hidden) { clearTimeout(timer); invalidate(); } else void refresh(); };
    const storage = (event: StorageEvent) => {
      if (event.key !== SCAN_PENDING_KEY && event.key !== null) return;
      storageBlocked = true; storageError = 'Saved scan requests changed in another tab. New scans are blocked here; reload this page to reconcile them.';
    };
    const unload = (event: BeforeUnloadEvent) => { if (busy) { event.preventDefault(); event.returnValue = ''; } };
    document.addEventListener('visibilitychange', visibility); window.addEventListener('storage', storage); window.addEventListener('beforeunload', unload);
    return () => {
      mounted = false; ++generation; clearTimeout(timer); onwrite?.('idle');
      document.removeEventListener('visibilitychange', visibility); window.removeEventListener('storage', storage); window.removeEventListener('beforeunload', unload);
    };
  });
</script>

<section class="scan-panel" aria-labelledby="scan-heading" aria-busy={checking || busy}>
  <h2 id="scan-heading">Scan downloaded files</h2>
  <p>Scan one file or folder on the server for {scope === 'tv' ? 'episodes of series' : 'movies'} already in the library, then import accepted files one at a time. Nothing is scanned until you choose Scan and import.</p>
  {#if domainChanged}
    <div class="notice" role="status"><p>The library type changed to {label(domain)}. This scan form still targets {label(scope)}.</p>
      <div class="actions"><button type="button" disabled={busy} onclick={() => keptDomain = domain}>Keep {label(scope)} scan form</button><button type="button" disabled={busy} onclick={discardAndSwitch}>Use {label(domain)} scans</button></div></div>
  {/if}
  <form onsubmit={(event) => { event.preventDefault(); void submit(); }}>
    <h3>New {label(scope)} scan</h3>
    <label>Downloaded file or folder<input aria-label="Downloaded file or folder" bind:value={path} required maxlength="4096" autocomplete="off" spellcheck="false" placeholder="/downloads/Release.Name" disabled={busy} aria-describedby="scan-path-help" /></label>
    <p id="scan-path-help" class="muted">An absolute server path. A path that does not exist finishes without importing anything.</p>
    <label>Scan transfer mode<select aria-label="Scan transfer mode" bind:value={mode} disabled={busy} aria-describedby="scan-mode-help">
      <option value="copy">Copy (keep the downloaded file)</option>
      <option value="move">Move (remove the downloaded file after import)</option>
      <option value="auto">Auto (may move files)</option>
    </select></label>
    <p id="scan-mode-help" class={mode === 'copy' ? 'muted' : 'warning'}>{mode === 'auto' ? 'Auto moves files for downloads that are not tracked by a download client, so the downloaded files are removed from this folder after import.' : mode === 'move' ? 'Move removes each imported file from the download folder. Folders and unimported files are left in place.' : 'Copy leaves every downloaded file in place.'}</p>
    <label>Scan priority<select aria-label="Scan priority" bind:value={priority} disabled={busy}><option value="normal">Normal</option><option value="high">High</option></select></label>
    <button class="primary" disabled={blocked || pending.length >= SCAN_PENDING_LIMIT}>Scan and import</button>
    {#if relatedWrite !== 'idle'}<p role="status">Another library write must finish before a new scan can start. Status checks continue.</p>{/if}
  </form>
  {#if notice}<p role="status">{notice}</p>{/if}
  {#if error}<p role="alert" class="error">{error}</p>{/if}
  {#if storageError}<p role="alert" class="error">{storageError}</p>{/if}

  {#if pending.length}
    <div class="pending">
      <h3>Scan requests awaiting confirmation</h3>
      <p>These requests were saved before sending, but the server's answer was not received. Checking reads the same request ID; nothing is re-sent automatically.</p>
      <ul>
        {#each pending as entry (entry.input.id)}
          {@const view = pendingViews[entry.input.id]}
          <li><article aria-label={`Unconfirmed scan ${entry.input.path}`}>
            <dl><dt>Path</dt><dd>{entry.input.path}</dd><dt>Library type</dt><dd>{label(entry.domain)}</dd><dt>Mode</dt><dd>{entry.input.mode} · {entry.input.priority} priority</dd><dt>Request ID</dt><dd><code>{entry.input.id}</code></dd></dl>
            {#if view?.state === 'checking'}<p role="status">Checking this request…</p>{:else if view?.message}<p role="status">{view.message}</p>{/if}
            <div class="actions">
              <button type="button" disabled={busy || view?.state === 'checking'} onclick={() => checkPending(entry)}>Check outcome</button>
              {#if view?.state === 'absent'}
                <button type="button" disabled={blocked} onclick={() => admit(entry, true)}>Send the same request again</button>
                <button type="button" disabled={busy} onclick={() => discardPending(entry)}>Forget request</button>
              {/if}
            </div>
          </article></li>
        {/each}
      </ul>
    </div>
  {/if}

  <div class="section-heading"><h3>Recent {label(scope)} scans</h3><button type="button" disabled={checking} onclick={() => { invalidate(); void refresh(); }}>Refresh scans</button></div>
  {#if liveStatus}<p role="status" class="live">{liveStatus}</p>{/if}
  {#if checking && !page}<p role="status">Loading scans…</p>{/if}
  {#if readError}<p role="alert" class="error">{readError} Retrying automatically with backoff.</p>{/if}
  {#if page}
    {#if page.items.length}
      <ol class="parents">
        {#each page.items as parent (parent.id)}
          <li><button type="button" aria-current={selectedId === parent.id ? 'true' : undefined} onclick={() => select(parent.id)}>
            <span class="path">{parent.path}</span>
            <small>{parent.status} · {scanPhaseText(parent.phase)}{parent.outcome ? ` · ${parent.outcome}` : ''} · {parent.counts.imported} imported · {parent.counts.rejected} rejected · {parent.counts.failed} failed</small>
          </button></li>
        {/each}
      </ol>
    {:else}<p class="empty">No {label(scope)} scans yet.</p>{/if}
    {#if page.total > page.limit}
      <div class="pagination"><button type="button" disabled={checking || parentOffset === 0} onclick={() => changeParentPage(parentOffset - SCAN_PARENT_PAGE)}>Previous scans</button><span>{page.offset + 1}–{Math.min(page.offset + page.limit, page.total)} of {page.total}</span><button type="button" disabled={checking || page.offset + page.limit >= page.total} onclick={() => changeParentPage(parentOffset + SCAN_PARENT_PAGE)}>Next scans</button></div>
    {/if}
  {/if}

  {#if selected}
    {@const parent = selected}
    <article class="detail" aria-label="Scan detail">
      <h3>Scan of {parent.path}</h3>
      <dl>
        <dt>Status</dt><dd>{parent.status} · {scanPhaseText(parent.phase)}{parent.outcome ? ` · outcome ${parent.outcome}` : ''}</dd>
        <dt>Summary</dt><dd>{describeScan(parent)}</dd>
        <dt>Mode</dt><dd>{parent.mode} · {parent.priority} priority</dd>
        <dt>Entries</dt><dd>{parent.counts.entries} found · {parent.counts.ready} ready · {parent.counts.importing} importing · {parent.counts.imported} imported · {parent.counts.already_imported} already imported · {parent.counts.rejected} rejected · {parent.counts.failed} failed · {parent.counts.cancelled} cancelled</dd>
        {#if parent.source}<dt>Source</dt><dd>{parent.source === 'missing' ? 'Path not found' : 'Found'}</dd>{/if}
        {#if describeCoverage(parent.coverage)}<dt>Tracked coverage</dt><dd>{describeCoverage(parent.coverage)}</dd>{/if}
        <dt>Source cleanup</dt><dd>{describeCleanup(parent.cleanup)}</dd>
        {#if parent.error_code}<dt>Error</dt><dd><code>{parent.error_code}</code></dd>{/if}
        <dt>Attempts</dt><dd>{parent.attempts} of 3</dd>
        <dt>Created</dt><dd>{when(parent.created_at)}</dd>
        {#if parent.completed_at}<dt>Finished</dt><dd>{when(parent.completed_at)}</dd>{/if}
        <dt>Command ID</dt><dd><code>{parent.id}</code></dd>
      </dl>
      {#if parent.cancel_requested && !isTerminalScan(parent.status)}<p role="status">Cancellation requested. {parent.counts.recovery_required ? 'A file that changed the library must be recovered explicitly before the scan can finish.' : 'The scan stops after the current file.'}</p>{/if}
      {#if parent.phase === 'recovery_required' && parent.cleanup.recovery_required}<p role="alert">Removing emptied source folders was interrupted after it started. Nothing is retried automatically; check the folder, then resume the cleanup explicitly. Only folders that are empty are ever removed.</p>
        {@const cleanupBody = cleanupResumeRequest(parent)}
        <div class="actions">
          {#if cleanupBody}<button type="button" class="primary" disabled={blocked || !fresh} onclick={() => resumeCleanup(parent)}>Resume source-folder cleanup</button>{/if}
          {#if lostCleanup && lostCleanup.id === parent.id}<button type="button" disabled={blocked} onclick={() => lostCleanup && resumeCleanup(parent, lostCleanup.body)}>Repeat identical cleanup request</button>{/if}
        </div>
      {:else if parent.phase === 'recovery_required'}<p role="alert">An import was interrupted after it started changing files. Review the failed entry and resume it explicitly; nothing retries automatically.</p>{/if}
      {#if canCancelScan(parent)}
        <div class="actions"><button type="button" disabled={blocked || !fresh} onclick={cancel}>Stop scan after current file</button></div>
        <p class="muted">Stopping is safe to repeat. A file that is already importing finishes; files not yet started are skipped and imported files stay imported.</p>
      {/if}
      {#if !fresh && !isTerminalScan(parent.status)}<p class="muted">Actions unlock after the next successful status read.</p>{/if}

      <div class="section-heading"><h4>Entries</h4>
        <label class="filter">Show scan entries<select aria-label="Show scan entries" value={childFilter} disabled={checking} onchange={(event) => changeFilter(event.currentTarget.value as ScanDisposition | '')}><option value="">All</option>{#each DISPOSITIONS as value (value)}<option value={value}>{value.replace('_', ' ')}</option>{/each}</select></label>
      </div>
      {#if !children}<p role="status">Loading entries…</p>
      {:else if !children.items.length}<p class="empty">{childFilter ? 'No entries with this result.' : parent.source === 'missing' ? 'The path was not found, so there are no entries.' : parent.inventory_revision === 0 ? isTerminalScan(parent.status) ? 'No entries were discovered.' : 'Entries appear once discovery is complete.' : 'This scan found no entries.'}</p>
      {:else}
        <ol class="children" start={children.offset + 1}>
          {#each children.items as child (child.id)}
            {@const body = resumeRequest(parent, child)}{@const lost = lostResumes[child.id]}
            <li><article aria-label={`Entry ${child.relative_path}`}>
              <h5 class="path">{child.relative_path}{child.kind !== 'file' ? ` (${child.kind})` : ''}</h5>
              <dl>
                <dt>Result</dt><dd>{child.disposition.replace('_', ' ')}{child.error_code ? ` · ${child.error_code}` : ''}</dd>
                {#if child.reasons.length}<dt>Reasons</dt><dd>{#each child.reasons as reason, index (index)}<code>{reason}</code>{index < child.reasons.length - 1 ? ', ' : ''}{/each}</dd>{/if}
                {#if child.destination}<dt>Destination</dt><dd>{child.destination}{child.resolved_mode ? ` (${child.resolved_mode})` : ''}</dd>{/if}
                {#if child.member_count > 1}<dt>Episodes</dt><dd>{child.member_count}</dd>{/if}
                {#if child.operation_phase}<dt>Import phase</dt><dd>{child.operation_phase}{child.path_failure_code ? ` · ${child.path_failure_code}` : ''}</dd>{/if}
                {#if child.recovery_required}<dt>Recovery</dt><dd>Required{child.resume_requested ? ' · requested' : ''}</dd>{/if}
              </dl>
              {#if body}<button type="button" class="primary" disabled={blocked || !fresh} onclick={() => resume(parent, child)}>Resume import of {child.relative_path}</button>{/if}
              {#if lost}<button type="button" disabled={blocked} onclick={() => resume(parent, child, lost)}>Repeat identical recovery request</button>{/if}
              {#if child.resume_requested}<p role="status">Recovery requested; the worker continues this same import.</p>{/if}
            </article></li>
          {/each}
        </ol>
        {#if children.total > children.limit}
          <div class="pagination"><button type="button" disabled={checking || childOffset === 0} onclick={() => changeChildPage(childOffset - SCAN_CHILD_PAGE)}>Previous entries</button><span>{children.offset + 1}–{Math.min(children.offset + children.limit, children.total)} of {children.total}</span><button type="button" disabled={checking || children.offset + children.limit >= children.total} onclick={() => changeChildPage(childOffset + SCAN_CHILD_PAGE)}>Next entries</button></div>
        {/if}
      {/if}
    </article>
  {/if}
</section>

<style>
  .scan-panel { background:#fff;border-top:3px solid #245ba8;padding:1.5rem;margin-top:1.5rem;min-width:0; }
  .scan-panel h2 { margin-top:0; }
  form, .notice, .pending { display:grid;gap:.75rem;margin-block:1rem; }
  .actions { display:flex;flex-wrap:wrap;gap:.5rem; }
  .warning { color:#7a4b00;background:#fff7e6;padding:.6rem;border-left:3px solid #b26b00; }
  .section-heading { display:flex;flex-wrap:wrap;align-items:center;justify-content:space-between;gap:.75rem; }
  .parents, .children, .pending ul { list-style:none;padding:0;margin:0;display:grid;gap:.5rem; }
  .parents button { width:100%;text-align:left;display:grid;gap:.2rem; }
  .path, dd { overflow-wrap:anywhere; }
  article { border:1px solid #cbd5de;border-radius:.4rem;padding:1rem;min-width:0; }
  .detail { margin-top:1rem; }
  h5 { font-size:.95rem;margin:0 0 .5rem; }
  dl { display:grid;grid-template-columns:minmax(6rem,auto) minmax(0,1fr);gap:.3rem 1rem; }
  dd { margin:0; }
  dt { font-weight:600; }
  code { overflow-wrap:anywhere; }
  .filter { display:flex;align-items:center;gap:.5rem;font-weight:600; }
  .filter select { width:auto; }
  .pagination { display:flex;flex-wrap:wrap;align-items:center;justify-content:space-between;gap:.5rem;margin-top:1rem; }
  .empty { color:#596b7b; }
  @media(max-width:600px) { .scan-panel{padding:1rem;} dl{grid-template-columns:1fr;} dd{margin-bottom:.4rem;} article{padding:.75rem;} }
</style>
