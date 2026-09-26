<script lang="ts">
  import { onMount } from 'svelte';
  import type { MediaDomain, RootFolder, UnmappedFolder, LookupResult, RescanCommand, RescanStatus } from './api.generated';
  import { listRootFolders, getRootFolder, createRootFolder, lookupLibrary, addLibrary, findLibraryByExternalId, createRescanCommand, getRescanCommand } from './api';

  type Phase = 'idle' | 'creating' | 'create_failed' | 'create_uncertain' | 'created' | 'rescanning' | 'rescan_failed' | 'succeeded';
  type Row = {
    key: string;
    folder: UnmappedFolder;
    included: boolean;
    term: string;
    searching: boolean;
    searched: boolean;
    searchError: string;
    results: LookupResult[];
    choice: LookupResult | null;
    skipped: boolean;
    phase: Phase;
    libraryItemId: number | null;
    createError: string;
    rescanId: string | null;
    rescanStatus: RescanStatus | null;
    rescanDetail: RescanCommand | null;
    rescanError: string;
  };

  let domain: MediaDomain = $state('tv');
  let step: 'roots' | 'folders' | 'summary' | 'progress' = $state('roots');

  let roots: RootFolder[] = $state([]);
  let rootsLoading = $state(false), rootsError = $state('');
  let newRootPath = $state(''), addingRoot = $state(false), addRootError = $state('');
  let selectedRoot: RootFolder | null = $state(null);

  let rows: Row[] = $state([]);
  let foldersLoading = $state(false), foldersError = $state('');
  let batchRunning = $state(false);

  let alive = true, rootsVersion = 0, foldersVersion = 0;
  let pollTimer: ReturnType<typeof setTimeout> | undefined;

  function freshRow(folder: UnmappedFolder): Row {
    return {
      key: folder.path, folder, included: false, term: folder.name, searching: false, searched: false, searchError: '', results: [], choice: null, skipped: false,
      phase: 'idle', libraryItemId: null, createError: '', rescanId: null, rescanStatus: null, rescanDetail: null, rescanError: '',
    };
  }
  function updateRow(key: string, patch: Partial<Row>) {
    rows = rows.map(row => row.key === key ? { ...row, ...patch } : row);
  }

  function rootErrorText(code: string | undefined, message: string): string {
    if (code === 'root_exists') return 'This path is already registered as a root folder.';
    if (code === 'root_unavailable_or_not_writable') return 'That path could not be confirmed as an accessible, writable directory.';
    return message;
  }
  function observationText(root: RootFolder): string {
    switch (root.observation) {
      case 'available': return root.unmapped_folders === null ? 'Observed, but folder contents are unknown.' : `${root.unmapped_folders!.length} unmapped folder(s) found.`;
      case 'busy': return 'Too many concurrent folder scans; try again shortly.';
      case 'timeout': return 'The folder scan timed out; try again.';
      case 'limited': return 'This folder has too many entries to list safely.';
      case 'inaccessible': return 'This path is not accessible right now.';
      default: return 'Unknown observation state.';
    }
  }

  async function loadRoots() {
    const version = ++rootsVersion, scope = domain;
    rootsLoading = true; rootsError = '';
    const result = await listRootFolders(scope);
    if (!alive || version !== rootsVersion) return;
    rootsLoading = false;
    if (result.ok) roots = result.data.items; else rootsError = result.error;
  }
  function changeDomain(next: MediaDomain) {
    if (batchRunning || domain === next) return;
    domain = next; step = 'roots'; selectedRoot = null; rows = []; foldersError = ''; rootsError = ''; addRootError = '';
    ++rootsVersion; ++foldersVersion;
    void loadRoots();
  }
  async function submitNewRoot() {
    if (addingRoot || !newRootPath.trim()) return;
    addingRoot = true; addRootError = '';
    const scope = domain;
    const result = await createRootFolder(scope, newRootPath.trim());
    if (!alive || scope !== domain) return;
    addingRoot = false;
    if (result.ok) { newRootPath = ''; await loadRoots(); }
    else addRootError = rootErrorText(result.code, result.error);
  }
  function chooseRoot(root: RootFolder) {
    if (batchRunning || root.unmapped_folders === null) return;
    selectedRoot = root;
    rows = root.unmapped_folders.map(freshRow);
    step = 'folders';
  }

  async function refreshFolders() {
    if (!selectedRoot || foldersLoading) return;
    const version = ++foldersVersion, scope = domain, id = selectedRoot.id;
    foldersLoading = true; foldersError = '';
    const result = await getRootFolder(scope, id);
    if (!alive || version !== foldersVersion) return;
    foldersLoading = false;
    if (!result.ok) { foldersError = result.error; return; }
    selectedRoot = result.data;
    if (result.data.unmapped_folders) {
      const freshList = result.data.unmapped_folders;
      const freshPaths = new Set(freshList.map(f => f.path));
      const existingByPath = new Map(rows.map(row => [row.folder.path, row]));
      const current = freshList.map(folder => existingByPath.get(folder.path) ?? freshRow(folder));
      const keepInProgress = rows.filter(row => !freshPaths.has(row.folder.path) && row.phase !== 'idle');
      rows = [...current, ...keepInProgress];
    } else {
      foldersError = `Folder contents could not be refreshed: ${observationText(result.data)}`;
    }
  }

  async function searchFolder(row: Row) {
    if (row.searching) return;
    const term = row.term.trim() || row.folder.name;
    updateRow(row.key, { searching: true, searchError: '', searched: false, results: [] });
    const scope = domain;
    const result = await lookupLibrary(scope, term);
    if (!alive || scope !== domain) return;
    if (result.ok) updateRow(row.key, { searching: false, searched: true, results: result.data.filter(item => item.media_type === scope) });
    else updateRow(row.key, { searching: false, searched: true, searchError: result.error });
  }
  function pickResult(row: Row, choice: LookupResult) {
    updateRow(row.key, { choice, skipped: false, included: true });
  }
  function changeMatch(row: Row) {
    updateRow(row.key, { choice: null, results: [], searched: false, searchError: '' });
  }
  function toggleSkip(row: Row) {
    updateRow(row.key, row.skipped ? { skipped: false } : { skipped: true, included: false, choice: null, results: [], searched: false });
  }
  function toggleIncluded(row: Row) {
    if (!row.choice) return;
    updateRow(row.key, { included: !row.included });
  }

  const importable = $derived(rows.filter(row => row.included && row.choice));
  // Only rows after the first with a given match are flagged: the first will actually be
  // created; later ones sharing its catalogue id will be rejected as duplicates.
  const duplicateRowKeys = $derived.by(() => {
    const seen = new Set<string>(), duplicateOf = new Set<string>();
    for (const row of importable) {
      const key = `${row.choice!.media_type}:${row.choice!.external_id}`;
      if (seen.has(key)) duplicateOf.add(row.key); else seen.add(key);
    }
    return duplicateOf;
  });
  function isDuplicate(row: Row): boolean {
    return duplicateRowKeys.has(row.key);
  }

  function schedulePoll() {
    clearTimeout(pollTimer);
    if (alive && !document.hidden && rows.some(row => row.phase === 'rescanning')) pollTimer = setTimeout(() => void pollRescans(), 5000);
  }
  function settleRescan(status: RescanStatus): Phase {
    if (status === 'succeeded' || status === 'skipped') return 'succeeded';
    if (status === 'failed' || status === 'cancelled') return 'rescan_failed';
    return 'rescanning';
  }
  async function fireRescan(row: Row) {
    const scope = domain;
    const result = await createRescanCommand(scope, row.libraryItemId!, 'normal');
    if (!alive || scope !== domain) return;
    if (!result.ok) { updateRow(row.key, { phase: 'rescan_failed', rescanError: `${result.error}${result.code ? ` (${result.code})` : ''}` }); return; }
    const command = result.data.commands[0];
    if (!command) { updateRow(row.key, { phase: 'rescan_failed', rescanError: 'Rescan request returned no command for this target.' }); return; }
    updateRow(row.key, { rescanId: command.id, rescanDetail: command, rescanStatus: command.status, phase: settleRescan(command.status) });
    schedulePoll();
  }
  async function retryRescan(row: Row) {
    updateRow(row.key, { phase: 'created', rescanError: '' });
    await fireRescan(row);
  }
  async function pollRescans() {
    if (!alive || document.hidden) return;
    const scope = domain;
    const pending = rows.filter(row => row.phase === 'rescanning' && row.rescanId);
    if (!pending.length) return;
    const results = await Promise.all(pending.map(row => getRescanCommand(scope, row.rescanId!)));
    if (!alive || scope !== domain) return;
    pending.forEach((row, index) => {
      const result = results[index];
      if (result.ok) updateRow(row.key, { rescanDetail: result.data, rescanStatus: result.data.status, phase: settleRescan(result.data.status) });
      else if (result.status === 404) updateRow(row.key, { phase: 'rescan_failed', rescanError: result.error });
      else updateRow(row.key, { rescanError: result.error });
    });
    schedulePoll();
  }

  async function createAndRescan(key: string) {
    const scope = domain;
    const row = rows.find(r => r.key === key);
    if (!row || !row.choice) return;
    updateRow(key, { phase: 'creating', createError: '' });
    const result = await addLibrary(scope, row.choice.external_id, row.folder.path);
    if (!alive || scope !== domain) return;
    if (result.ok) {
      updateRow(key, { phase: 'created', libraryItemId: result.data.id });
      const created = rows.find(r => r.key === key);
      if (created) await fireRescan(created);
    } else if (result.status === undefined) {
      updateRow(key, { phase: 'create_uncertain', createError: `${result.error} The outcome is unknown. Use "Check current state" below before retrying; this folder was not retried automatically.` });
    } else {
      updateRow(key, { phase: 'create_failed', createError: `${result.error}${result.code ? ` (${result.code})` : ''}` });
    }
  }
  async function runImport() {
    if (batchRunning) return;
    batchRunning = true; step = 'progress';
    const scope = domain;
    const targets = rows.filter(row => row.included && row.choice && row.phase === 'idle').map(row => row.key);
    for (const key of targets) {
      if (!alive || scope !== domain) break;
      await createAndRescan(key);
    }
    if (alive && scope === domain) batchRunning = false;
  }

  async function reconcileUncertain(row: Row) {
    if (!selectedRoot) return;
    const scope = domain, id = selectedRoot.id;
    const result = await getRootFolder(scope, id);
    if (!alive || scope !== domain) return;
    if (!result.ok) { updateRow(row.key, { createError: `${row.createError} Reload failed: ${result.error}` }); return; }
    selectedRoot = result.data;
    if (result.data.unmapped_folders === null) {
      updateRow(row.key, { createError: `Root folder observation is still uncertain (${observationText(result.data)}). Try checking again shortly.` });
      return;
    }
    const stillUnmapped = result.data.unmapped_folders.some(folder => folder.path === row.folder.path);
    if (stillUnmapped) { updateRow(row.key, { phase: 'idle', createError: '' }); return; }
    if (!row.choice) return;
    const found = await findLibraryByExternalId(scope, row.choice.external_id);
    if (!alive || scope !== domain) return;
    // The tvdb_id/tmdb_id filter alone does not prove this is *our* create: require the path to
    // match too, so a same-id entry someone else added is never mistaken for this uncertain write.
    const item = found.ok ? found.data.items.find(candidate => normalizePath(candidate.path) === normalizePath(row.folder.path)) : undefined;
    if (!item) {
      updateRow(row.key, { createError: 'This folder no longer appears as unmapped, but no library entry at this exact path could be found automatically. Check your library manually before retrying; do not retry blindly.' });
      return;
    }
    updateRow(row.key, { phase: 'created', libraryItemId: item.id, createError: '' });
    const created = rows.find(r => r.key === row.key);
    if (created) await fireRescan(created);
  }
  function normalizePath(value: string): string {
    return value.replace(/\/+$/, '');
  }
  async function runSingle(action: () => Promise<void>) {
    if (batchRunning) return;
    batchRunning = true;
    const scope = domain;
    await action();
    if (alive && scope === domain) batchRunning = false;
  }

  function phaseLabel(row: Row): string {
    switch (row.phase) {
      case 'idle': return 'Not started';
      case 'creating': return 'Creating library entry…';
      case 'create_failed': return 'Failed to create';
      case 'create_uncertain': return 'Outcome unknown';
      case 'created': return 'Created; starting file scan…';
      case 'rescanning': return `Scanning files (${row.rescanStatus ?? 'queued'})…`;
      case 'rescan_failed': return 'File scan did not complete';
      case 'succeeded': return 'Done';
      default: return row.phase;
    }
  }

  onMount(() => {
    alive = true; void loadRoots();
    const visible = () => { clearTimeout(pollTimer); if (!document.hidden) void pollRescans(); };
    document.addEventListener('visibilitychange', visible);
    return () => { alive = false; ++rootsVersion; ++foldersVersion; clearTimeout(pollTimer); document.removeEventListener('visibilitychange', visible); };
  });
</script>

<section aria-labelledby="import-existing-heading" class="import-existing">
  <div class="heading">
    <div>
      <h1 id="import-existing-heading">Import existing library</h1>
      <p>Point hrrdarr at a root folder that already contains series or movie folders on disk, match each one to a catalogue title, and add it. Adding creates the library entry only; a rescan then associates the folder's existing files.</p>
    </div>
    <nav aria-label="Import domain"><button aria-pressed={domain === 'tv'} disabled={batchRunning} onclick={() => changeDomain('tv')}>TV</button><button aria-pressed={domain === 'movies'} disabled={batchRunning} onclick={() => changeDomain('movies')}>Movies</button></nav>
  </div>

  <ol class="steps" aria-label="Import steps">
    <li aria-current={step === 'roots' ? 'step' : undefined}>1. Root folder</li>
    <li aria-current={step === 'folders' ? 'step' : undefined}>2. Match folders</li>
    <li aria-current={step === 'summary' ? 'step' : undefined}>3. Review</li>
    <li aria-current={step === 'progress' ? 'step' : undefined}>4. Import</li>
  </ol>

  {#if step === 'roots'}
    <div class="step-body">
      {#if rootsLoading}<p role="status">Loading root folders…</p>{/if}
      {#if rootsError}<p role="alert" class="error">{rootsError}</p><button onclick={loadRoots}>Retry loading root folders</button>{/if}
      {#if !rootsLoading && !rootsError}
        {#if roots.length === 0}<p>No root folders registered for {domain === 'tv' ? 'TV' : 'movies'} yet. Add one below.</p>{/if}
        <ul class="collection">
          {#each roots as root (root.id)}
            <li>
              <button aria-pressed={selectedRoot?.id === root.id} disabled={batchRunning || root.unmapped_folders === null} onclick={() => chooseRoot(root)}>
                <span>{root.path}<small>{observationText(root)}</small></span>
              </button>
            </li>
          {/each}
        </ul>
      {/if}
      <form onsubmit={(event) => { event.preventDefault(); void submitNewRoot(); }}>
        <label>Add a root folder<input bind:value={newRootPath} required maxlength="4096" placeholder={domain === 'tv' ? '/library/tv' : '/library/movies'} /></label>
        {#if addRootError}<p role="alert" class="error">{addRootError}</p>{/if}
        <button class="primary" disabled={addingRoot}>{addingRoot ? 'Checking folder…' : 'Add root folder'}</button>
      </form>
    </div>
  {/if}

  {#if step === 'folders' && selectedRoot}
    <div class="step-body">
      <div class="section-heading"><h2>{selectedRoot.path}</h2><button disabled={foldersLoading} onclick={refreshFolders}>{foldersLoading ? 'Refreshing…' : 'Refresh folder list'}</button></div>
      {#if foldersError}<p role="alert" class="error">{foldersError}</p>{/if}
      {#if rows.length === 0}
        <p>No unmapped folders under this root. Every folder here already belongs to your library.</p>
      {:else}
        <ul class="rows">
          {#each rows as row (row.key)}
            <li class="row">
              <div class="row-head">
                <label class="check"><input type="checkbox" checked={row.included} disabled={!row.choice} onchange={() => toggleIncluded(row)} />{row.folder.relative_path}</label>
                {#if row.skipped}<span class="skip-tag">Skipped</span>{/if}
              </div>
              {#if row.skipped}
                <button onclick={() => toggleSkip(row)}>Undo skip for {row.folder.name}</button>
              {:else if row.choice}
                <p>Matched: <strong>{row.choice.title}</strong> {row.choice.year ?? ''} <small>{domain === 'tv' ? 'TVDB' : 'TMDB'} {row.choice.external_id}</small></p>
                <button onclick={() => changeMatch(row)}>Change match for {row.folder.name}</button>
              {:else}
                <form onsubmit={(event) => { event.preventDefault(); void searchFolder(row); }}>
                  <label>Search catalogue for {'"'}{row.folder.name}{'"'}<input bind:value={row.term} maxlength="256" /></label>
                  <button disabled={row.searching}>{row.searching ? 'Searching…' : `Search for ${row.folder.name}`}</button>
                </form>
                {#if row.searchError}<p role="alert" class="error">{row.searchError}</p>{/if}
                {#if row.searched && !row.searching && row.results.length === 0 && !row.searchError}<p>No matches. Try a different title or catalogue ID, or skip this folder.</p>{/if}
                {#if row.results.length > 0}
                  <ul class="lookup-results">
                    {#each row.results as result (`${result.media_type}:${result.external_id}`)}
                      <li><button onclick={() => pickResult(row, result)}>{result.title} {result.year ?? ''}<small>{domain === 'tv' ? 'TVDB' : 'TMDB'} {result.external_id}</small></button></li>
                    {/each}
                  </ul>
                {/if}
                <button onclick={() => toggleSkip(row)}>Skip {row.folder.name}, no match</button>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
      <div class="actions">
        <button onclick={() => { step = 'roots'; selectedRoot = null; rows = []; }}>Back to root folders</button>
        <button class="primary" disabled={importable.length === 0} onclick={() => step = 'summary'}>Review {importable.length} selected folder(s)</button>
      </div>
    </div>
  {/if}

  {#if step === 'summary'}
    <div class="step-body">
      <h2>Review before importing</h2>
      {#if importable.length === 0}
        <p>No folders are matched and included yet.</p>
      {:else}
        <p>{importable.length} folder(s) will be created in {domain === 'tv' ? 'TV' : 'movie'} library and immediately rescanned for existing files.</p>
        <table>
          <thead><tr><th scope="col">Folder</th><th scope="col">Matched title</th></tr></thead>
          <tbody>
            {#each importable as row (row.key)}
              <tr>
                <td>{row.folder.path}</td>
                <td>{row.choice!.title} {row.choice!.year ?? ''} <small>{domain === 'tv' ? 'TVDB' : 'TMDB'} {row.choice!.external_id}</small>
                  {#if isDuplicate(row)}<p role="alert" class="error">Another selected folder matches the same title. Only the first will be created; this one will fail as a duplicate.</p>{/if}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
      <div class="actions">
        <button onclick={() => step = 'folders'}>Back to folder matching</button>
        <button class="primary" disabled={importable.length === 0} onclick={runImport}>Start import</button>
      </div>
    </div>
  {/if}

  {#if step === 'progress'}
    <div class="step-body">
      <div class="section-heading"><h2>Import progress</h2><button disabled={foldersLoading} onclick={refreshFolders}>{foldersLoading ? 'Refreshing…' : 'Refresh root folder observation'}</button></div>
      {#if foldersError}<p role="alert" class="error">{foldersError}</p>{/if}
      <ul class="rows">
        {#each importable as row (row.key)}
          <li class="row">
            <p><strong>{row.folder.path}</strong> → {row.choice!.title} {row.choice!.year ?? ''}</p>
            <p role="status">{phaseLabel(row)}</p>
            {#if row.phase === 'create_failed' || row.phase === 'create_uncertain'}<p role="alert" class="error">{row.createError}</p>{/if}
            {#if row.phase === 'idle' || row.phase === 'create_failed'}<button disabled={batchRunning} onclick={() => void runSingle(() => createAndRescan(row.key))}>Retry create for {row.folder.name}</button>{/if}
            {#if row.phase === 'create_uncertain'}<button disabled={batchRunning} onclick={() => void runSingle(() => reconcileUncertain(row))}>Check whether {row.folder.name} was created</button>{/if}
            {#if row.phase === 'rescan_failed'}
              <p role="alert" class="error">{row.rescanError || `Rescan ${row.rescanStatus ?? 'did not complete'}${row.rescanDetail?.skip_reason ? `: ${row.rescanDetail.skip_reason}` : ''}${row.rescanDetail?.error_code ? ` (${row.rescanDetail.error_code})` : ''}`}</p>
              <button disabled={batchRunning} onclick={() => void runSingle(() => retryRescan(row))}>Retry file scan for {row.folder.name}</button>
            {/if}
            {#if row.phase === 'succeeded'}
              <p>File scan {row.rescanStatus}. {row.rescanDetail?.files_adopted ?? 0} file(s) adopted, {row.rescanDetail?.files_removed ?? 0} removed.</p>
            {/if}
          </li>
        {/each}
      </ul>
      <div class="actions"><button disabled={batchRunning} onclick={() => { step = 'folders'; }}>Back to folder matching</button></div>
    </div>
  {/if}
</section>

<style>
  .import-existing{background:#fff;border:1px solid #cbd5de;padding:1.5rem;margin:1rem 0}
  .heading{display:flex;justify-content:space-between;align-items:start;gap:1rem;flex-wrap:wrap}
  .steps{display:flex;gap:1rem;list-style:none;padding:0;margin:1rem 0;flex-wrap:wrap;font-size:.85rem;color:#596b7b}
  .steps li[aria-current=step]{color:#173f79;font-weight:700}
  .step-body{margin-top:1rem}
  .section-heading{display:flex;align-items:center;justify-content:space-between;gap:1rem;flex-wrap:wrap}
  .row{border-top:1px solid #dce3e9;padding:1rem 0}
  .row-head{display:flex;align-items:center;gap:.8rem;flex-wrap:wrap}
  .check{display:flex;gap:.5rem;align-items:center;font-weight:600}
  .check input{width:auto}
  .skip-tag{font-size:.8rem;color:#596b7b;border:1px solid #cbd5de;border-radius:3px;padding:.1rem .4rem}
  .lookup-results{list-style:none;margin:.5rem 0;padding:0;max-width:620px}
  .lookup-results button{width:100%;text-align:left;margin-bottom:.4rem}
  .actions{display:flex;gap:.6rem;flex-wrap:wrap;margin-top:1rem}
  .collection{list-style:none;margin:0 0 1rem;padding:0}
  .collection li{border-bottom:1px solid #dce3e9}
  .collection button{width:100%;display:flex;align-items:center;justify-content:space-between;text-align:left;border:0;border-radius:0;gap:.8rem;padding:1rem .5rem}
  table{border-collapse:collapse;width:100%;margin-top:.5rem}
  th,td{padding:.6rem;border-bottom:1px solid #dce3e9;text-align:left;vertical-align:top}
  @media(max-width:700px){.import-existing{padding:1rem}}
</style>
