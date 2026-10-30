<script lang="ts">
  // Wanted lists: read-only pages from the native Wanted API plus explicit search admission.
  // Nothing here retries a write automatically; an uncertain search is resent only as the SAME
  // request after the user asks, which the server reads back instead of admitting twice.
  import { onMount, untrack } from 'svelte';
  import type { ApiPage, ClientSelection, MediaDomain, MovieWantedItem, Provider, SearchCommand, TvWantedItem, WantedKind, WantedPage, WantedSearchInput, WantedSearchReceipt } from './api.generated';
  import { createWantedSearch, getSearchCommand, listProviders, listWanted, monitorEpisodes, monitorMovies } from './api';
  import { eligibleDownloadClient, indexerScope, isIndexer } from './provider-draft';
  import { WANTED_PAGE_SIZES, MAX_WANTED_SEARCH_TARGETS, availabilityLabel, cutoffLabel, defaultOptions, episodeLabel, formatDate, lastSearchLabel, monitoredLabel, receiptSummary, retainVisible, searchBody, selectedIds, sortChoices, wantedQuery, type WantedOptions } from './wanted-view';
  type Item = TvWantedItem | MovieWantedItem;
  let { kind, onkind }: { kind: WantedKind; onkind: (kind: WantedKind) => void } = $props();

  let domain = $state<MediaDomain>('tv');
  let options = $state<WantedOptions>(defaultOptions('tv'));
  let page = $state<WantedPage<Item> | null>(null), loading = $state(false), loadError = $state('');
  let selection = $state<Set<number>>(new Set());
  let providers = $state<ApiPage<Provider> | null>(null), providerOffset = $state(0), providerError = $state(''), providerLoading = $state(false);
  let indexerId = $state(''), clientId = $state('');
  let searching = $state(false), pending = $state<WantedSearchInput | null>(null), uncertain = $state(false);
  let searchError = $state(''), receipt = $state<WantedSearchReceipt | null>(null), confirmAll = $state(false);
  let statuses = $state<SearchCommand[]>([]), statusLoading = $state(false), statusError = $state('');
  let monitoring = $state(false), monitorNotice = $state(''), monitorError = $state('');
  let readVersion = 0, alive = true;

  const noun = $derived(domain === 'tv' ? 'episodes' : 'movies');
  const listName = $derived(kind === 'missing' ? `Missing ${noun}` : `Cutoff unmet ${noun}`);
  const visibleIds = $derived(page?.items.map((item) => item.target.id) ?? []);
  const allVisibleSelected = $derived(visibleIds.length > 0 && visibleIds.every((id) => selection.has(id)));
  const indexer = $derived(providers?.items.find((p) => p.id === indexerId) ?? null);
  const client = $derived(providers?.items.find((p) => p.id === clientId) ?? null);
  const indexers = $derived(providers?.items.filter((p) => p.enabled && isIndexer(p) && indexerScope(p, domain)?.enable_automatic_search !== false && indexerScope(p, domain) !== null) ?? []);
  const clients = $derived(providers?.items.filter((p) => eligibleDownloadClient(p, domain, indexer)) ?? []);
  const searchReady = $derived(!!indexer && (!clientId || !!client) && !searching && !uncertain && pending === null);
  const busy = $derived(loading || searching || monitoring);

  async function load() {
    const version = ++readVersion, scope = domain, list = kind, query = wantedQuery(scope, list, options);
    loading = true; loadError = '';
    const result = await listWanted<Item>(scope, list, query);
    if (!alive || version !== readVersion) return;
    loading = false;
    if (!result.ok) { loadError = result.error; return; }
    if (result.data.media_type !== scope || result.data.kind !== list) { loadError = 'Unexpected Wanted response for this list.'; return; }
    page = result.data;
    selection = retainVisible(selection, result.data.items.map((item) => item.target.id));
  }
  async function loadProviders(offset = 0) {
    providerLoading = true; providerError = '';
    const result = await listProviders(offset);
    if (!alive) return;
    providerLoading = false;
    if (result.ok) { providers = result.data; providerOffset = offset; } else providerError = result.error;
  }
  function resetScope() {
    ++readVersion; page = null; selection = new Set(); confirmAll = false; monitorNotice = ''; monitorError = '';
  }
  function changeDomain(next: MediaDomain) {
    if (next === domain || busy || pending !== null) return;
    domain = next; options = defaultOptions(next); indexerId = ''; clientId = '';
    receipt = null; statuses = []; searchError = ''; resetScope(); void load();
  }
  function update(patch: Partial<WantedOptions>, keepOffset = false) {
    if (busy) return;
    options = { ...options, ...patch, ...(keepOffset ? {} : { offset: 0 }) };
    if (!keepOffset) selection = new Set();
    void load();
  }
  function toggle(id: number) {
    const next = new Set(selection);
    if (next.has(id)) next.delete(id); else next.add(id);
    selection = next;
  }
  function toggleAll() {
    selection = allVisibleSelected ? new Set() : new Set(visibleIds);
  }
  function clientSelection(): ClientSelection {
    return client ? { kind: 'explicit', provider_id: client.id, provider_revision: client.revision } : { kind: 'default' };
  }
  async function submit(body: WantedSearchInput) {
    searching = true; searchError = ''; confirmAll = false; pending = body;
    const result = await createWantedSearch(domain, body);
    if (!alive) return;
    searching = false;
    if (result.ok) {
      receipt = result.data; pending = null; uncertain = false; selection = new Set();
      await readStatuses(); void load();
      return;
    }
    searchError = `${result.error}${result.code ? ` (${result.code})` : ''}`;
    // A missing status, server failure or timeout does not prove nothing was admitted.
    uncertain = !result.status || result.status >= 500;
    if (!uncertain) pending = null;
  }
  function searchSelected() {
    if (!indexer || !searchReady) return;
    const ids = selectedIds(selection);
    if (!ids.length || ids.length > MAX_WANTED_SEARCH_TARGETS) return;
    void submit(searchBody(crypto.randomUUID(), kind, { kind: 'ids', ids }, indexer, clientSelection()));
  }
  function searchAll() {
    if (!indexer || !searchReady) return;
    const scope = domain === 'movies' && kind === 'missing' && options.availability !== 'all'
      ? { kind: 'all' as const, monitored: options.monitored, availability: options.availability }
      : { kind: 'all' as const, monitored: options.monitored };
    void submit(searchBody(crypto.randomUUID(), kind, scope, indexer, clientSelection()));
  }
  async function readStatuses() {
    if (!receipt) return;
    statusLoading = true; statusError = '';
    const shown = receipt.commands.slice(0, 25), rows: SearchCommand[] = [];
    for (const command of shown) {
      const result = await getSearchCommand(command.command_id);
      if (!alive) return;
      if (result.ok) rows.push(result.data); else { statusError = result.error; break; }
    }
    statusLoading = false; statuses = rows;
  }
  async function setMonitored(monitored: boolean) {
    const ids = selectedIds(selection);
    if (!ids.length || busy) return;
    monitoring = true; monitorNotice = ''; monitorError = '';
    const result = domain === 'tv' ? await monitorEpisodes(ids, monitored) : await monitorMovies(ids, monitored);
    if (!alive) return;
    monitoring = false;
    if (!result.ok) { monitorError = `${result.error} Reload the list before another change.`; return; }
    monitorNotice = `${monitored ? 'Monitored' : 'Unmonitored'} ${ids.length} ${ids.length === 1 ? noun.slice(0, -1) : noun}.${domain === 'tv' && monitored ? ' Episodes under an unmonitored series stay effectively unmonitored.' : ''}`;
    selection = new Set(); void load();
  }
  const statusCounts = $derived(statuses.reduce<Record<string, number>>((counts, row) => ({ ...counts, [row.status]: (counts[row.status] ?? 0) + 1 }), {}));
  const tv = (item: Item): item is TvWantedItem => item.target.media_type === 'episode';
  const movie = (item: Item): item is MovieWantedItem => item.target.media_type === 'movie';
  const rowName = (item: Item) => tv(item) ? `${item.series_title} ${episodeLabel(item)} ${item.title}` : `${item.title} (${item.year ?? 'year unknown'})`;
  const diagnostics = $derived.by(() => {
    const d = page?.diagnostics; if (!d) return [];
    return [[d.unknown_air_date, 'unknown air date'], [d.unknown_series_runtime, 'unknown series runtime'], [d.unknown_year, 'unknown year'], [d.unknown_cutoff, 'unknown current quality or cutoff']]
      .filter((entry): entry is [number, string] => typeof entry[0] === 'number' && entry[0] > 0);
  });

  $effect(() => { kind; untrack(() => { resetScope(); options = { ...defaultOptions(domain), monitored: options.monitored }; receipt = null; statuses = []; void load(); }); });
  onMount(() => { alive = true; void loadProviders(); return () => { alive = false; ++readVersion; }; });
</script>

<section class="wanted" aria-labelledby="wanted-heading">
  <h1 id="wanted-heading">Wanted</h1>
  <div class="bar">
    <nav aria-label="Wanted lists"><button aria-pressed={kind === 'missing'} disabled={busy} onclick={() => onkind('missing')}>Missing</button><button aria-pressed={kind === 'cutoff'} disabled={busy} onclick={() => onkind('cutoff')}>Cutoff unmet</button></nav>
    <nav aria-label="Wanted media type"><button aria-pressed={domain === 'tv'} disabled={busy || pending !== null} onclick={() => changeDomain('tv')}>TV</button><button aria-pressed={domain === 'movies'} disabled={busy || pending !== null} onclick={() => changeDomain('movies')}>Movies</button></nav>
  </div>
  <p class="muted">{kind === 'missing'
    ? domain === 'tv' ? 'Episodes without an associated file whose air time plus series runtime has passed. Specials are included.' : 'Library movies with a known year and no associated file, including movies not yet available.'
    : `${domain === 'tv' ? 'Episodes (excluding specials)' : 'Movies'} whose current file quality is below the profile cutoff. Custom format scores do not affect this list.`}</p>

  <form class="filters" aria-label="Wanted filters" onsubmit={(event) => event.preventDefault()}>
    <label>Monitoring<select aria-label="Monitoring filter" value={options.monitored} disabled={busy} onchange={(e) => update({ monitored: e.currentTarget.value as WantedOptions['monitored'] })}><option value="monitored">Monitored</option><option value="unmonitored">Unmonitored</option><option value="all">All</option></select></label>
    <label>Sort by<select aria-label="Sort by" value={options.sort} disabled={busy} onchange={(e) => update({ sort: e.currentTarget.value as WantedOptions['sort'] })}>{#each sortChoices(domain, kind) as choice (choice.value)}<option value={choice.value}>{choice.label}</option>{/each}</select></label>
    <label>Direction<select aria-label="Sort direction" value={options.direction} disabled={busy} onchange={(e) => update({ direction: e.currentTarget.value as WantedOptions['direction'] })}><option value="asc">Ascending</option><option value="desc">Descending</option></select></label>
    {#if domain === 'movies' && kind === 'missing'}<label>Availability<select aria-label="Availability filter" value={options.availability} disabled={busy} onchange={(e) => update({ availability: e.currentTarget.value as WantedOptions['availability'] })}><option value="all">All</option><option value="available">Available</option><option value="unavailable">Not yet available</option></select></label>{/if}
    <label>Page size<select aria-label="Page size" value={String(options.limit)} disabled={busy} onchange={(e) => update({ limit: Number(e.currentTarget.value) })}>{#each WANTED_PAGE_SIZES as size (size)}<option value={String(size)}>{size}</option>{/each}</select></label>
    <button type="button" disabled={busy} onclick={() => load()}>Reload list</button>
  </form>

  {#if loading}<p role="status">Loading {listName.toLowerCase()}…</p>{/if}
  {#if loadError}<p role="alert" class="error">{loadError}</p><button disabled={busy} onclick={() => load()}>Retry loading list</button>{/if}
  {#if diagnostics.length}<p role="note">Not listed because a required fact is unknown: {diagnostics.map(([count, label]) => `${count} with ${label}`).join('; ')}.</p>{/if}

  {#if page}
    <p class="muted" aria-live="polite">{page.total === 0 ? `No ${listName.toLowerCase()} for this filter.` : `Showing ${page.offset + 1}–${Math.min(page.offset + page.items.length, page.total)} of ${page.total} ${listName.toLowerCase()}.`} {selection.size} selected on this page.</p>
    <div class="table-scroll"><table aria-label={listName} aria-busy={loading}>
      <thead><tr>
        <th scope="col"><input type="checkbox" aria-label="Select all on this page" checked={allVisibleSelected} disabled={busy || !visibleIds.length} onchange={toggleAll} /></th>
        {#if domain === 'tv'}<th scope="col">Series</th><th scope="col">Episode</th><th scope="col">Title</th><th scope="col">Air date</th>
        {:else}<th scope="col">Title</th><th scope="col">Year</th><th scope="col">Releases</th>{#if kind === 'missing'}<th scope="col">Availability</th>{/if}{/if}
        <th scope="col">Last searched</th><th scope="col">Monitoring</th>{#if kind === 'cutoff'}<th scope="col">Current quality</th>{/if}
      </tr></thead>
      <tbody>
        {#each page.items as item (`${item.target.media_type}:${item.target.id}`)}
          <tr>
            <td><input type="checkbox" aria-label={`Select ${rowName(item)}`} checked={selection.has(item.target.id)} disabled={busy} onchange={() => toggle(item.target.id)} /></td>
            {#if tv(item)}
              <td>{item.series_title}</td><td>{episodeLabel(item)}</td><td>{item.title}</td><td>{formatDate(item.air_date_utc)}</td>
            {:else if movie(item)}
              <td>{item.title}</td><td>{item.year ?? 'Unknown'}</td><td><small>Cinemas {formatDate(item.in_cinemas)} · Digital {formatDate(item.digital_release)} · Physical {formatDate(item.physical_release)}</small></td>{#if kind === 'missing'}<td>{availabilityLabel(item)}</td>{/if}
            {/if}
            <td>{#if item.last_search_time}<time datetime={item.last_search_time}>{lastSearchLabel(item)}</time>{:else}{lastSearchLabel(item)}{/if}</td>
            <td>{monitoredLabel(item)}</td>{#if kind === 'cutoff'}<td>{cutoffLabel(item)}{#if item.file}<small class="path">File {item.file.id}</small>{/if}</td>{/if}
          </tr>
        {:else}
          <tr><td colspan={domain === 'tv' && kind === 'cutoff' ? 8 : 7}>{loading ? 'Loading…' : `Nothing ${kind === 'missing' ? 'missing' : 'below cutoff'} here.`}</td></tr>
        {/each}
      </tbody>
    </table></div>
    <div class="pagination">
      <button disabled={busy || page.offset === 0} onclick={() => update({ offset: Math.max(0, options.offset - options.limit) }, true)}>Previous Wanted page</button>
      <button disabled={busy || page.offset + page.limit >= page.total} onclick={() => update({ offset: options.offset + options.limit }, true)}>Next Wanted page</button>
    </div>
    <div class="actions" aria-label="Selection actions" role="group">
      <button disabled={busy || !selection.size} onclick={() => setMonitored(true)}>Monitor selected</button>
      <button disabled={busy || !selection.size} onclick={() => setMonitored(false)}>Unmonitor selected</button>
    </div>
    {#if monitoring}<p role="status">Saving monitoring…</p>{/if}{#if monitorNotice}<p role="status">{monitorNotice}</p>{/if}{#if monitorError}<p role="alert" class="error">{monitorError}</p>{/if}
  {/if}

  <section class="search" aria-label="Wanted search">
    <h2>Search</h2>
    <p>Searches run as automatic user searches: the best accepted release is downloaded. User searches bypass background availability and delay rules. Last searched updates once the indexer has been queried, even when it answers with an error; RSS sync and grabs do not change it.{domain === 'tv' ? ' Each episode is searched individually; season packs are not grouped yet.' : ''}</p>
    {#if providerLoading}<p role="status">Loading providers…</p>{/if}{#if providerError}<p role="alert" class="error">{providerError}</p>{/if}
    <fieldset disabled={searching || uncertain || pending !== null}>
      <legend>Search providers</legend>
      <label>Wanted search indexer<select bind:value={indexerId} onchange={() => { clientId = ''; }}><option value="">Choose an indexer</option>{#each indexers as provider (provider.id)}<option value={provider.id}>{provider.name}</option>{/each}</select></label>
      <label>Wanted download client<select bind:value={clientId}><option value="">Default — choose from eligible clients</option>{#each clients as provider (provider.id)}<option value={provider.id}>{provider.name}</option>{/each}</select></label>
      {#if providers}<div class="actions"><button type="button" disabled={providerLoading || providerOffset === 0} onclick={() => loadProviders(Math.max(0, providerOffset - 25))}>Previous providers</button><button type="button" disabled={providerLoading || providerOffset + providers.limit >= providers.total} onclick={() => loadProviders(providerOffset + 25)}>Next providers</button><button type="button" disabled={providerLoading} onclick={() => { indexerId = ''; clientId = ''; void loadProviders(); }}>Reload providers</button></div>{/if}
      <div class="actions">
        <button disabled={!searchReady || !selection.size} onclick={searchSelected}>Search selected ({selection.size})</button>
        <button disabled={!searchReady || !page || page.total === 0} onclick={() => { confirmAll = true; }}>Search all</button>
      </div>
    </fieldset>
    {#if confirmAll && page}
      <div role="alertdialog" aria-labelledby="wanted-confirm-title" aria-describedby="wanted-confirm-body" class="confirm">
        <h3 id="wanted-confirm-title">Search all {listName.toLowerCase()}?</h3>
        <p id="wanted-confirm-body">This queues an automatic search for every current {page.monitored === 'all' ? '' : `${page.monitored} `}member of this list ({page.total} now; at most {MAX_WANTED_SEARCH_TARGETS}), not only this page, oldest-searched first (never-searched targets first). Targets already being searched are skipped. Requests larger than the limit or the free command capacity are refused as a whole.</p>
        <button class="primary" disabled={!searchReady} onclick={searchAll}>Confirm search all</button><button onclick={() => { confirmAll = false; }}>Cancel search all</button>
      </div>
    {/if}
    {#if searching}<p role="status">Submitting search request…</p>{/if}
    {#if searchError}<p role="alert" class="error">{searchError}</p>{/if}
    {#if uncertain && pending}<p role="alert">The search request may have been admitted. Resending the same request reads back its original result instead of searching twice.</p><button onclick={() => submit(pending!)}>Retry same search request</button><button onclick={() => { pending = null; uncertain = false; searchError = ''; }}>Discard pending request</button>{/if}
    {#if receipt}
      <div role="status" class="receipt"><p>{receiptSummary(receipt)}</p><p class="muted">Request {receipt.request_id}</p></div>
      {#if receipt.commands.length}
        <button disabled={statusLoading} onclick={readStatuses}>Check search status</button>
        {#if statusLoading}<p role="status">Reading search status…</p>{/if}{#if statusError}<p role="alert" class="error">{statusError}</p>{/if}
        {#if statuses.length}<p>Status of {statuses.length}{receipt.commands.length > statuses.length ? ` of ${receipt.commands.length}` : ''} searches: {Object.entries(statusCounts).map(([status, count]) => `${count} ${status}`).join(', ')}. Inspect Activity for downloads.</p>{/if}
        <ul aria-label="Queued searches">{#each receipt.commands.slice(0, 25) as command (command.command_id)}<li>{command.target.media_type} {command.target.id}: {statuses.find((row) => row.id === command.command_id)?.status ?? 'queued'}{statuses.find((row) => row.id === command.command_id)?.error_code ? ` (${statuses.find((row) => row.id === command.command_id)?.error_code})` : ''}</li>{/each}</ul>
      {/if}
    {/if}
  </section>
</section>

<style>
  .wanted{background:#fff;border:1px solid #cbd5de;padding:1.5rem}
  .bar{display:flex;flex-wrap:wrap;gap:1rem;justify-content:space-between}
  .filters{display:flex;flex-wrap:wrap;gap:.8rem;align-items:end;max-width:none}
  .filters label{min-width:150px}
  .table-scroll{overflow-x:auto}
  table{border-collapse:collapse;width:100%;text-align:left}
  th,td{padding:.6rem .5rem;border-bottom:1px solid #dce3e9;vertical-align:top}
  th{font-size:.85rem}
  td small{display:block}
  .pagination,.actions{display:flex;flex-wrap:wrap;gap:.6rem;margin:1rem 0}
  .search{border-top:2px solid #245ba8;margin-top:1.5rem;padding-top:1rem}
  .confirm{border:2px solid #a12637;padding:1rem;margin:1rem 0;background:#fff8f8}
  .confirm button{margin-right:.6rem}
  .receipt{background:#eef6ee;padding:.6rem .9rem;border-left:3px solid #2f7d32}
  ul{padding-left:1.25rem}li{overflow-wrap:anywhere}
  @media(max-width:800px){.wanted{padding:1rem}.filters label{min-width:120px}}
</style>
