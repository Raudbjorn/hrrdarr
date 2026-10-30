<script lang="ts">
  // Movie collections: browse, monitor, edit the add-policy and refresh from the metadata service.
  // Superseded reads are ignored, writes are serialised through one write state, and the
  // refresh poll never overlaps itself and stops on unmount.
  import { onMount } from 'svelte';
  import type { CollectionPatch, CollectionRefreshCommand, MovieCollection, QualityProfileSummary, RootFolder, Tag } from './api.generated';
  import type { Result, SettingsWriteState } from './api';
  import {
    bulkUpdateCollections, getCollection, getCollectionRefresh, listCollectionRefreshes, listCollections, listQualityProfiles,
    listRootFolders, listTags, startCollectionRefresh, updateCollection,
  } from './api';
  import {
    AVAILABILITIES, AVAILABILITY_LABELS, POLL_INTERVAL_MS, POLL_MAX_MS, countsLabel, draftDirty, draftOf, isActive, lastSyncLabel,
    libraryMonitorPatch, listQuery, monitorPatch, pageCount, policyPatch, policySummary, refreshSummary, statusText, toggleId,
    type MonitorFilter, type PolicyDraft,
  } from './collections-view';

  const LOOKUP_PAGE = 50, LOOKUP_MAX_PAGES = 10;

  let items = $state<MovieCollection[]>([]), total = $state(0), page = $state(0);
  let filter = $state<MonitorFilter>('all'), queryText = $state(''), appliedText = $state('');
  let loading = $state(false), loadedOnce = $state(false), listError = $state('');
  let selected = $state<number[]>([]);
  let profiles = $state<QualityProfileSummary[]>([]), roots = $state<RootFolder[]>([]), tags = $state<Tag[]>([]), lookupError = $state('');
  let detailId = $state<number | null>(null), detail = $state<MovieCollection | null>(null), detailLoading = $state(false), detailError = $state('');
  let draft = $state<PolicyDraft>({ qualityProfileId: '', rootFolderId: '', minimumAvailability: '', searchOnAdd: false, tagIds: [] });
  let write = $state<SettingsWriteState>('idle'), writeError = $state(''), notice = $state('');
  let refresh = $state<CollectionRefreshCommand | null>(null), refreshError = $state(''), refreshStarting = $state(false), watching = $state(false);
  let listVersion = 0, detailVersion = 0, pollTimer: ReturnType<typeof setTimeout> | undefined, pollVersion = 0, alive = true;

  const profileNames = $derived(new Map(profiles.map(p => [p.id, p.name] as [number, string])));
  const pages = $derived(pageCount(total));
  const idle = $derived(write === 'idle');
  const refreshRunning = $derived(refreshStarting || watching);
  const dirty = $derived(detail !== null && draftDirty(draft, detail));
  const members = $derived(detail?.movies ?? []);
  const inLibrary = $derived(members.filter(m => m.movie_id !== null));
  const missing = $derived(members.filter(m => m.movie_id === null));

  async function loadAll<T>(fetchPage: (offset: number) => Promise<Result<{ items: T[]; total: number }>>): Promise<Result<T[]>> {
    const out: T[] = [];
    for (let i = 0; i < LOOKUP_MAX_PAGES; i++) {
      const result = await fetchPage(i * LOOKUP_PAGE);
      if (!result.ok) return result;
      out.push(...result.data.items);
      if (out.length >= result.data.total || result.data.items.length === 0) break;
    }
    return { ok: true, data: out };
  }
  async function loadLookups() {
    lookupError = '';
    const [p, r, t] = await Promise.all([loadAll(o => listQualityProfiles('movies', o)), loadAll(o => listRootFolders('movies', o)), listTags('movies')]);
    if (!alive) return;
    if (p.ok) profiles = p.data; else lookupError = `Quality profiles could not be loaded: ${p.error}`;
    if (r.ok) roots = r.data; else lookupError ||= `Root folders could not be loaded: ${r.error}`;
    if (t.ok) tags = t.data; else lookupError ||= `Tags could not be loaded: ${t.error}`;
  }

  async function loadList() {
    const version = ++listVersion;
    loading = true; listError = '';
    const result = await listCollections(listQuery(filter, appliedText, page));
    if (!alive || version !== listVersion) return;
    loading = false;
    if (!result.ok) { listError = `Collections could not be loaded: ${result.error}`; return; }
    if (result.data.items.length === 0 && result.data.total > 0 && page > 0) { page = pageCount(result.data.total) - 1; void loadList(); return; }
    items = result.data.items; total = result.data.total; loadedOnce = true;
    const visible = new Set(items.map(i => i.id));
    selected = selected.filter(id => visible.has(id));
  }

  async function loadDetail(id: number, resetDraft: boolean) {
    const version = ++detailVersion;
    detailLoading = true; detailError = '';
    const result = await getCollection(id);
    if (!alive || version !== detailVersion) return;
    detailLoading = false;
    if (!result.ok) { detailError = `Collection could not be loaded: ${result.error}`; return; }
    detail = result.data;
    if (resetDraft) draft = draftOf(result.data);
  }
  function openDetail(id: number) {
    if (detailId === id) { detailId = null; detail = null; ++detailVersion; detailLoading = false; return; }
    detailId = id; detail = null; writeError = ''; notice = '';
    void loadDetail(id, true);
  }
  async function reloadAfterWrite(resetDraft: boolean) {
    const jobs = [loadList()];
    if (detailId !== null) jobs.push(loadDetail(detailId, resetDraft));
    await Promise.all(jobs);
  }

  /** Runs one write; HTTP rejections show the server message, transport failures leave the outcome uncertain. */
  async function doWrite<T>(run: () => Promise<Result<T>>, done: string, resetDraft = false): Promise<boolean> {
    if (write !== 'idle') return false;
    write = 'busy'; writeError = ''; notice = '';
    const result = await run();
    if (!alive) return false;
    if (!result.ok) {
      writeError = result.error;
      if (result.status === undefined) { write = 'uncertain'; writeError = `Write outcome unknown (${result.error}). Reload collections and check before writing again.`; }
      else write = 'idle';
      if (result.status !== undefined) await reloadAfterWrite(false);
      return false;
    }
    // Keep the editor locked until readback has replaced the submitted draft.
    await reloadAfterWrite(resetDraft);
    if (!alive) return false;
    write = 'idle'; notice = done;
    return true;
  }
  async function reconcile() {
    writeError = ''; notice = '';
    await reloadAfterWrite(true);
    if (listError === '' && detailError === '') write = 'idle';
  }

  const toggleMonitor = async (c: MovieCollection, on: boolean, input: HTMLInputElement) => {
    const ok = await doWrite(() => updateCollection(c.id, monitorPatch(on)), `${c.title} ${on ? 'monitored' : 'unmonitored'}.`);
    if (!ok && alive) input.checked = c.monitored;
  };
  const savePolicy = () => detail && doWrite(() => updateCollection(detail!.id, policyPatch(draft)), 'Collection settings saved.', true);
  const monitorLibrary = (on: boolean) => detail && doWrite(() => updateCollection(detail!.id, libraryMonitorPatch(on)), on ? 'Library movies monitored.' : 'Library movies unmonitored.');
  const bulk = (on: boolean) => {
    const ids = [...selected], patch: CollectionPatch = monitorPatch(on);
    return doWrite(() => bulkUpdateCollections(ids, patch), `${ids.length} collection${ids.length === 1 ? '' : 's'} ${on ? 'monitored' : 'unmonitored'}.`);
  };

  function applyFilter(event?: Event) {
    event?.preventDefault();
    appliedText = queryText; page = 0; selected = [];
    void loadList();
  }
  function goTo(next: number) { page = Math.min(Math.max(0, next), pages - 1); void loadList(); }
  const setSelected = (id: number, on: boolean) => { selected = toggleId(selected, id, on); };
  const allSelected = $derived(items.length > 0 && items.every(i => selected.includes(i.id)));
  const selectAll = (on: boolean) => { selected = on ? items.map(i => i.id) : []; };

  // Refresh command: single in-flight poll, rescheduled only after the previous read finishes.
  function stopPolling() { ++pollVersion; clearTimeout(pollTimer); pollTimer = undefined; watching = false; }
  function watch(initial: CollectionRefreshCommand) {
    stopPolling();
    const version = pollVersion, deadline = Date.now() + POLL_MAX_MS;
    refresh = initial; watching = isActive(initial.status);
    const settle = (cmd: CollectionRefreshCommand) => { watching = false; refresh = cmd; void reloadAfterWrite(false); };
    if (!watching) return;
    const tick = async () => {
      const result = await getCollectionRefresh(initial.id);
      if (!alive || version !== pollVersion) return;
      if (!result.ok) { refreshError = `Refresh status could not be read: ${result.error}`; watching = false; return; }
      refreshError = '';
      if (!isActive(result.data.status)) { settle(result.data); return; }
      refresh = result.data;
      if (Date.now() >= deadline) { watching = false; refreshError = 'Still running; stopped watching. Reload to check again.'; return; }
      pollTimer = setTimeout(tick, POLL_INTERVAL_MS);
    };
    pollTimer = setTimeout(tick, POLL_INTERVAL_MS);
  }
  async function startRefresh() {
    if (refreshRunning) return;
    refreshStarting = true; refreshError = '';
    const result = await startCollectionRefresh();
    if (!alive) return;
    refreshStarting = false;
    if (!result.ok) { refreshError = `Refresh could not be started: ${result.error}`; return; }
    watch(result.data);
  }
  async function resumeRefresh() {
    const result = await listCollectionRefreshes(1);
    if (!alive || !result.ok) return;
    const latest = result.data.items[0];
    if (latest && isActive(latest.status)) watch(latest);
  }

  onMount(() => {
    void loadList(); void loadLookups(); void resumeRefresh();
    return () => { alive = false; ++listVersion; ++detailVersion; stopPolling(); };
  });
</script>

<section aria-label="Collections" aria-busy={loading}>
  <h2>Collections</h2>
  <p>Movie collections group related movies. Monitoring a collection needs a quality profile, a root folder and a minimum availability; it adds the collection's known missing movies to the library, and later refreshes add new ones. There is no artwork. “Search on add” is recorded and counted as pending searches, but searches are not submitted yet.</p>

  <div role="group" aria-label="Refresh collections">
    <button onclick={startRefresh} disabled={refreshRunning}>Refresh collections</button>
    {#if refreshStarting}<p role="status">Starting refresh…</p>{/if}
    {#if refresh}<p role="status" aria-live="polite" data-refresh-status={refresh.status}>{refreshSummary(refresh)}{#if watching} Checking again every {POLL_INTERVAL_MS / 1000} s.{/if}</p>{/if}
    {#if refreshError}<p role="alert">{refreshError}</p>{/if}
  </div>

  <form role="search" aria-label="Filter collections" onsubmit={applyFilter}>
    <label>Search collections <input type="search" bind:value={queryText} maxlength="100" /></label>
    <label>Show
      <select bind:value={filter} onchange={() => applyFilter()}>
        <option value="all">All collections</option><option value="monitored">Monitored</option><option value="unmonitored">Unmonitored</option>
      </select>
    </label>
    <button>Search</button>
  </form>

  {#if lookupError}<p role="alert">{lookupError}</p>{/if}
  {#if writeError}<p role="alert">{writeError}</p>{/if}
  {#if notice}<p role="status">{notice}</p>{/if}
  {#if write === 'busy'}<p role="status">Saving…</p>{/if}
  {#if write === 'uncertain'}<button onclick={reconcile}>Reload collections</button>{/if}

  <div role="group" aria-label="Bulk actions">
    <label><input type="checkbox" checked={allSelected} disabled={items.length === 0} onchange={e => selectAll(e.currentTarget.checked)} /> Select all on this page</label>
    <button disabled={!idle || selected.length === 0} onclick={() => bulk(true)}>Monitor selected</button>
    <button disabled={!idle || selected.length === 0} onclick={() => bulk(false)}>Unmonitor selected</button>
    <span>{selected.length} selected</span>
  </div>

  {#if loading && !loadedOnce}<p role="status">Loading collections…</p>{/if}
  {#if listError}<p role="alert">{listError} <button onclick={loadList}>Retry loading collections</button></p>{/if}
  {#if loadedOnce && !listError}
    {#if items.length === 0}
      <p role="status">{appliedText || filter !== 'all' ? 'No collections match this filter.' : 'No collections yet. Collections appear as movies with collection metadata are added or refreshed.'}</p>
    {:else}
      <ul aria-label="Collection list">
        {#each items as c (c.id)}
          <li>
            <input type="checkbox" aria-label={`Select ${c.title}`} checked={selected.includes(c.id)} onchange={e => setSelected(c.id, e.currentTarget.checked)} />
            <strong>{c.title}</strong>
            <label><input type="checkbox" aria-label={`Monitor ${c.title}`} checked={c.monitored} disabled={!idle} onchange={e => toggleMonitor(c, e.currentTarget.checked, e.currentTarget)} /> Monitored</label>
            <span>{countsLabel(c)}</span> · <span>{lastSyncLabel(c.last_info_sync)}</span> · <span>{policySummary(c, profileNames)}</span>
            <button aria-expanded={detailId === c.id} onclick={() => openDetail(c.id)}>{detailId === c.id ? 'Hide' : 'Details for'} {c.title}</button>
          </li>
        {/each}
      </ul>
      <nav aria-label="Collection pages">
        <button disabled={page === 0 || loading} onclick={() => goTo(page - 1)}>Previous page</button>
        <span>Page {page + 1} of {pages} ({total} collection{total === 1 ? '' : 's'})</span>
        <button disabled={page + 1 >= pages || loading} onclick={() => goTo(page + 1)}>Next page</button>
      </nav>
    {/if}
  {/if}

  {#if detailId !== null}
    <section aria-label="Collection details">
      {#if detailLoading && !detail}<p role="status">Loading collection…</p>{/if}
      {#if detailError}<p role="alert">{detailError} <button onclick={() => loadDetail(detailId!, true)}>Retry loading collection</button></p>{/if}
      {#if detail}
        <h3>{detail.title}</h3>
        {#if detail.overview}<p>{detail.overview}</p>{/if}
        <p>{detail.monitored ? 'Monitored' : 'Not monitored'} · {countsLabel(detail)} · {lastSyncLabel(detail.last_info_sync)}</p>

        <form aria-label="Collection settings" onsubmit={e => { e.preventDefault(); void savePolicy(); }}>
          <fieldset disabled={!idle}>
            <legend>Add policy</legend>
            <label>Quality profile
              <select bind:value={draft.qualityProfileId}><option value="">Not set</option>{#each profiles as p (p.id)}<option value={String(p.id)}>{p.name}</option>{/each}</select>
            </label>
            <label>Root folder
              <select bind:value={draft.rootFolderId}><option value="">Not set</option>{#each roots as r (r.id)}<option value={String(r.id)}>{r.path}</option>{/each}</select>
            </label>
            <label>Minimum availability
              <select bind:value={draft.minimumAvailability}><option value="">Not set</option>{#each AVAILABILITIES as a}<option value={a}>{AVAILABILITY_LABELS[a]}</option>{/each}</select>
            </label>
            <label><input type="checkbox" bind:checked={draft.searchOnAdd} /> Search on add (recorded and counted as pending; searches are not submitted yet)</label>
            <fieldset>
              <legend>Tags applied to added movies</legend>
              {#each tags as t (t.id)}
                <label><input type="checkbox" checked={draft.tagIds.includes(t.id)} onchange={e => { draft.tagIds = toggleId(draft.tagIds, t.id, e.currentTarget.checked); }} /> {t.label}</label>
              {:else}<span>No movie tags exist.</span>{/each}
            </fieldset>
            <button disabled={!dirty}>Save settings</button>
            {#if dirty}<span role="status">Unsaved changes</span>{/if}
          </fieldset>
        </form>

        <div role="group" aria-label="Library movie monitoring">
          <button disabled={!idle} onclick={() => monitorLibrary(true)}>Monitor library movies</button>
          <button disabled={!idle} onclick={() => monitorLibrary(false)}>Unmonitor library movies</button>
          <span>One-shot: changes monitoring of this collection's movies already in the library.</span>
        </div>

        <h4>In library ({inLibrary.length})</h4>
        <ul aria-label="Movies in library">
          {#each inLibrary as m (m.metadata_id)}<li>{m.title}{m.year ? ` (${m.year})` : ''} · movie {m.movie_id} · {m.monitored ? 'monitored' : 'unmonitored'}</li>{:else}<li>None of this collection's movies are in the library.</li>{/each}
        </ul>
        <h4>Missing ({missing.length})</h4>
        <ul aria-label="Missing movies">
          {#each missing as m (m.metadata_id)}<li>{m.title}{m.year ? ` (${m.year})` : ''} · not in library</li>{:else}<li>No missing movies.</li>{/each}
        </ul>
      {/if}
    </section>
  {/if}
</section>
