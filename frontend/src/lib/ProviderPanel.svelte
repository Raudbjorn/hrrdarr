<script lang="ts">
  import { onMount } from 'svelte';
  import type { ApiPage, DownloadScope, Provider, ProviderImplementation, ProviderInput, ProviderSettings, ProviderTemplate, ProviderTestResult } from './api.generated';
  import { listProviders, getProvider, getProviderSchema, createProvider, updateProvider, deleteProvider, testProvider } from './api';
  let page: ApiPage<Provider> | null = $state(null), templates: ProviderTemplate[] = $state([]), selected: Provider | null = $state(null);
  let editing = $state(false), implementation: ProviderImplementation = $state('torznab'), name = $state(''), endpoint = $state(''), enabled = $state(true), priority = $state(1);
  let tv = $state(true), movies = $state(false), tvCategories = $state(''), animeCategories = $state(''), movieCategories = $state(''), tvCategory = $state(''), movieCategory = $state('');
  let credentialAction = $state('preserve'), credentialKind = $state<'api_key' | 'username_password'>('api_key'), apiKey = $state(''), username = $state(''), password = $state('');
  let loading = $state(false), busy = $state(false), dirty = $state(false), confirmingDelete = $state(false), conflict = $state(false), uncertain = $state(false);
  let error = $state(''), notice = $state(''), testResult: ProviderTestResult | null = $state(null);
  let alive = true, selectionVersion = 0, listVersion = 0;
  const template = $derived(templates.find(item => item.implementation === implementation));
  function clearSecrets() { apiKey = ''; username = ''; password = ''; }
  function fill(item: Provider | null) {
    selected = item; editing = true; confirmingDelete = false; conflict = false; uncertain = false; error = ''; notice = ''; testResult = null; clearSecrets(); credentialAction = 'preserve';
    if (item) {
      implementation = item.settings.implementation; name = item.name; endpoint = item.settings.endpoint; enabled = item.enabled; priority = item.priority; tv = !!item.settings.tv; movies = !!item.settings.movies;
      if (item.settings.implementation === 'qbittorrent') {tvCategory = item.settings.tv?.category ?? ''; movieCategory = item.settings.movies?.category ?? ''; tvCategories = ''; animeCategories = ''; movieCategories = '';}
      else {tvCategories = item.settings.tv?.categories.join(', ') ?? ''; animeCategories = item.settings.tv?.anime_categories.join(', ') ?? ''; movieCategories = item.settings.movies?.categories.join(', ') ?? ''; tvCategory = ''; movieCategory = '';}
    } else {name = ''; endpoint = ''; tv = true; movies = false; tvCategory = ''; movieCategory = ''; defaults();}
    credentialKind = implementation === 'qbittorrent' ? 'username_password' : 'api_key';
    dirty = !item;
  }
  function defaults() {
    const current = templates.find(item => item.implementation === implementation);
    enabled = current?.enabled ?? true; priority = current?.priority ?? 1;
    tvCategories = current?.defaults.kind === 'indexer' ? current.defaults.tv.categories.join(', ') : '';
    animeCategories = current?.defaults.kind === 'indexer' ? current.defaults.tv.anime_categories.join(', ') : '';
    movieCategories = current?.defaults.kind === 'indexer' ? current.defaults.movies.categories.join(', ') : '';
  }
  function changeImplementation() {clearSecrets(); credentialKind = implementation === 'qbittorrent' ? 'username_password' : 'api_key'; credentialAction = 'preserve'; defaults(); dirty = true;}
  async function load(offset = 0) {
    const version = ++listVersion; loading = true;
    const result = await listProviders(offset);
    if (!alive || version !== listVersion) return;
    loading = false; if (result.ok) page = result.data; else error = result.error;
  }
  async function select(id: string) {
    if (busy) return;
    const version = ++selectionVersion; busy = true; error = ''; clearSecrets();
    const result = await getProvider(id);
    if (!alive || version !== selectionVersion) return;
    busy = false; if (result.ok) fill(result.data); else error = result.error;
  }
  function categories(value: string): number[] | null {
    if (!value.trim()) return [];
    const parts = value.split(',').map(part => part.trim());
    if (parts.length > 64 || parts.some(part => !/^[1-9]\d*$/.test(part))) return null;
    const numbers = parts.map(Number);
    return numbers.some(id => !Number.isSafeInteger(id) || id > 2147483647) || new Set(numbers).size !== numbers.length ? null : numbers;
  }
  function downloadScope(category: string, existing: DownloadScope | null | undefined): DownloadScope | null {
    if (existing) return {...existing, category};
    if (template?.defaults.kind !== 'download_client') return null;
    const {kind: _, ...defaults} = template.defaults;
    return {...defaults, category};
  }
  function input(): ProviderInput | null {
    if (!tv && !movies) {error = 'Enable at least one media scope.'; return null;}
    let settings: ProviderSettings;
    if (implementation === 'qbittorrent') {
      const saved = selected?.settings.implementation === 'qbittorrent' ? selected.settings : null;
      const tvScope = tv ? downloadScope(tvCategory, saved?.tv) : null, movieScope = movies ? downloadScope(movieCategory, saved?.movies) : null;
      if ((tv && !tvScope) || (movies && !movieScope)) {error = 'Provider defaults are unavailable. Reload provider settings.'; return null;}
      settings = {implementation, endpoint, tv:tvScope, movies:movieScope};
    } else {
      const tvIds = categories(tvCategories), animeIds = categories(animeCategories), movieIds = categories(movieCategories);
      if ((tv && (!tvIds || !animeIds)) || (movies && !movieIds)) {error = 'Categories must be distinct positive numeric IDs separated by commas (maximum 64 per field).'; return null;}
      const saved = selected && selected.settings.implementation !== 'qbittorrent' ? selected.settings : null;
      const defaults = template?.defaults.kind === 'indexer' ? template.defaults : null;
      if ((!saved?.tv && tv || !saved?.movies && movies) && !defaults) {error = 'Provider defaults are unavailable. Reload provider settings.'; return null;}
      settings = {implementation, endpoint, tv:tv ? {...(saved?.tv ?? defaults!.tv), categories:tvIds!, anime_categories:animeIds!} : null, movies:movies ? {...(saved?.movies ?? defaults!.movies), categories:movieIds!} : null};
    }
    if (selected?.has_credentials && endpoint !== selected.settings.endpoint && credentialAction === 'preserve') {error = 'The endpoint changed. Explicitly replace or clear credentials before saving to another destination.'; return null;}
    return {name, enabled, priority, settings, ...(credentialAction === 'clear' ? {credentials:null} : credentialAction === 'replace' ? {credentials:implementation === 'qbittorrent' && credentialKind === 'username_password' ? {kind:'username_password' as const,username,password} : {kind:'api_key' as const,api_key:apiKey}} : {})};
  }
  async function save() {
    if (busy || conflict || uncertain) return;
    error = ''; notice = ''; const body = input(); if (!body) return;
    const version = selectionVersion; busy = true;
    const result = selected ? await updateProvider(selected.id, {...body, revision:selected.revision}) : await createProvider(body);
    clearSecrets();
    if (!alive || version !== selectionVersion) return;
    busy = false;
    if (result.ok) {fill(result.data); notice = 'Provider saved. Test it to check the saved connection.'; void load(page?.offset ?? 0);}
    else {error = result.error; conflict = result.status === 409; uncertain = !result.status; if (uncertain) error += ' The response was lost or unreadable. Refresh the list and reload the saved provider before another save; the change may have committed.';}
  }
  async function test() {
    if (!selected || busy || dirty || conflict || uncertain) return;
    const id = selected.id, revision = selected.revision, version = selectionVersion; busy = true; error = ''; notice = ''; testResult = null;
    const result = await testProvider(id);
    if (!alive || version !== selectionVersion) return;
    // Read back even failed/uncertain tests: a response timeout does not erase an observation.
    const current = await getProvider(id);
    if (!alive || version !== selectionVersion) return;
    busy = false;
    if (current.ok) {
      selected = current.data;
      if (current.data.revision !== revision) {conflict = true; error = 'Provider changed during testing. Reload the saved provider before editing or testing again.'; return;}
    }
    if (result.ok && result.data.provider_id === id && result.data.revision === revision) {testResult = result.data; notice = 'Saved connection test completed.';}
    else error = result.ok ? 'Test result did not match the selected provider revision.' : `${result.error} The saved observation below may describe an earlier test; no successful result is assumed.`;
    if (!current.ok) error += `${error ? ' ' : ''}Could not refresh the saved observation. Reload the saved provider to check it.`;
    void load(page?.offset ?? 0);
  }
  async function remove() {
    if (!selected || busy || !confirmingDelete || conflict || uncertain) return;
    const id = selected.id, revision = selected.revision, version = selectionVersion; busy = true; error = ''; clearSecrets();
    const result = await deleteProvider(id, revision);
    if (!alive || version !== selectionVersion) return;
    busy = false; confirmingDelete = false;
    if (result.ok) {selected = null; editing = false; notice = 'Provider configuration deleted. Media and remote downloads were not deleted.'; void load();}
    else {error = result.error; conflict = result.status === 409; uncertain = !result.status; if (uncertain) error += ' Deletion may have completed. Refresh the list before retrying.';}
  }
  onMount(() => {
    alive = true; void load();
    void Promise.all([getProviderSchema('tv','indexer'),getProviderSchema('tv','download_client')]).then(results => {
      if (!alive) return;
      templates = results.flatMap(result => result.ok ? result.data.templates : []);
      const failed = results.find(result => !result.ok); if (failed && !failed.ok) error = failed.error;
    });
    return () => {alive = false; ++selectionVersion; ++listVersion; clearSecrets();};
  });
</script>

<section aria-labelledby="providers-heading">
  <div class="heading"><div><h1 id="providers-heading">Providers</h1><p>Configure indexers and download clients for TV, movies or both.</p></div><button class="primary" disabled={busy || !templates.length} onclick={() => {++selectionVersion; implementation = 'torznab'; fill(null);}}>New provider</button></div>
  {#if error}<p class="error" role="alert">{error}</p>{/if}{#if notice}<p role="status">{notice}</p>{/if}
  <div class="providers-workspace">
    <aside aria-label="Saved providers"><button disabled={loading} onclick={() => load(page?.offset ?? 0)}>Refresh providers</button>{#if loading}<p role="status">Loading providers…</p>{/if}
      {#if page}<p>{page.total} configured</p><ul>{#each page.items as provider (provider.id)}<li><button disabled={busy} aria-current={selected?.id === provider.id ? 'true' : undefined} onclick={() => select(provider.id)}><strong>{provider.name}</strong><span>{provider.settings.implementation} / {provider.settings.tv ? 'TV' : ''}{provider.settings.tv && provider.settings.movies ? ' + ' : ''}{provider.settings.movies ? 'Movies' : ''}</span><span>{provider.enabled ? 'Enabled' : 'Disabled'} · {provider.test_status.replaceAll('_',' ')}</span></button></li>{:else}<li>No providers yet. Add an indexer or download client to begin.</li>{/each}</ul><div class="pagination"><button disabled={loading || page.offset === 0} onclick={() => load(Math.max(0,page!.offset - 25))}>Previous providers</button><button disabled={loading || page.offset + page.limit >= page.total} onclick={() => load(page!.offset + page!.limit)}>Next providers</button></div>{/if}
    </aside>
    <article aria-label="Provider editor">
      {#if busy}<p role="status">Working on provider…</p>{/if}
      {#if editing}
        <h2>{selected ? selected.name : 'New provider'}</h2>
        {#if selected}<p>Saved revision {selected.revision}. Credentials: {selected.has_credentials ? 'stored (write-only)' : 'none stored'}.</p><p>Saved test: {selected.test_status.replaceAll('_',' ')}{selected.last_test ? ` at ${new Date(selected.last_test.tested_at * 1000).toLocaleString()} (revision ${selected.last_test.revision})` : ''}{selected.last_test?.error_code ? `: ${selected.last_test.error_code}` : ''}.</p><button disabled={busy} onclick={() => select(selected!.id)}>Reload saved provider</button>{/if}
        <form onsubmit={(event) => {event.preventDefault(); void save();}} oninput={() => dirty = true} onchange={() => {dirty = true; confirmingDelete = false; testResult = null;}}>
          <fieldset disabled={busy || conflict || uncertain}><legend>Connection</legend>
            <label>Provider type<select bind:value={implementation} disabled={!!selected} onchange={changeImplementation}><option value="torznab">Torznab</option><option value="newznab">Newznab</option><option value="qbittorrent">qBittorrent</option></select></label>
            <label>Name<input bind:value={name} required maxlength="128" /></label><label>Endpoint<input type="url" bind:value={endpoint} required maxlength="2048" placeholder="https://indexer.example/api" /></label>
            <p>Use an endpoint without credentials. Saving does not contact it; Test saved provider contacts every configured scope.</p>
            <label class="check"><input type="checkbox" bind:checked={enabled} />Enabled</label><label>Priority<input type="number" bind:value={priority} min="1" max="100" step="1" required /></label>
          </fieldset>
          <fieldset disabled={busy || conflict || uncertain}><legend>Media scopes</legend>
            <label class="check"><input type="checkbox" bind:checked={tv} />TV scope</label>
            {#if tv}{#if implementation === 'qbittorrent'}<label>TV download category<input bind:value={tvCategory} required maxlength="64" /></label>{:else}<label>TV categories<input bind:value={tvCategories} placeholder="5000" /></label><label>Anime categories<input bind:value={animeCategories} placeholder="5070" /></label>{/if}{/if}
            <label class="check"><input type="checkbox" bind:checked={movies} />Movie scope</label>
            {#if movies}{#if implementation === 'qbittorrent'}<label>Movie download category<input bind:value={movieCategory} required maxlength="64" /></label>{:else}<label>Movie categories<input bind:value={movieCategories} required placeholder="2000" /></label>{/if}{/if}
            <p>{implementation === 'qbittorrent' ? 'Use distinct, non-overlapping categories for TV and movies. Existing imported categories, priorities and download options are preserved.' : 'Enter the numeric category IDs advertised by your indexer, separated by commas. Existing search options are preserved.'} Disabling a scope removes its saved settings.</p>
          </fieldset>
          <fieldset disabled={busy || conflict || uncertain}><legend>Credentials</legend>
            <label>Credential action<select bind:value={credentialAction} onchange={clearSecrets}><option value="preserve">{selected ? 'Preserve stored credentials' : 'No credentials'}</option><option value="replace">Replace entire credential bundle</option><option value="clear">Clear entire credential bundle</option></select></label>
            {#if credentialAction !== 'preserve'}<p class="credential-warning">{credentialAction === 'clear' ? 'Clearing removes' : 'Replacing discards'} the entire saved credential bundle, including any private TV/movie parameters not shown here.</p>{/if}
            {#if credentialAction === 'replace'}{#if implementation === 'qbittorrent'}<label>Credential type<select bind:value={credentialKind} onchange={clearSecrets}><option value="username_password">Username and password</option><option value="api_key">API key</option></select></label>{/if}{#if implementation === 'qbittorrent' && credentialKind === 'username_password'}<label>Username<input bind:value={username} autocomplete="off" required maxlength="4096" /></label><label>Password<input type="password" bind:value={password} autocomplete="new-password" required maxlength="4096" /></label>{:else}<label>API key<input type="password" bind:value={apiKey} autocomplete="new-password" required maxlength="4096" /></label>{/if}{/if}
          </fieldset>
          <button class="primary" disabled={busy || conflict || uncertain || !dirty || (!selected && !template)}>Save provider</button>
        </form>
        {#if selected}<div class="actions"><button disabled={busy || dirty || conflict || uncertain || !selected.test_supported} onclick={test}>Test saved provider</button><button disabled={busy || conflict || uncertain} onclick={() => confirmingDelete = !confirmingDelete}>Delete provider</button></div>{#if dirty}<p>Save or reload changes before testing. Tests always use the saved configuration.</p>{/if}{/if}
        {#if confirmingDelete}<div class="delete-confirm"><p>Delete configuration for {selected?.name}? This removes both saved scopes and credentials. It does not delete media or remote downloads.</p><button disabled={busy} onclick={remove}>Confirm delete configuration</button><button disabled={busy} onclick={() => confirmingDelete = false}>Cancel delete</button></div>{/if}
        {#if testResult}<div role="status"><h3>Connection test result</h3><p>Tested revision {testResult.revision}: {testResult.result.domains.join(', ')}.</p>{#if 'missing_categories' in testResult.result}<p>qBittorrent {testResult.result.application_version}; API {testResult.result.api_version}. Queueing {testResult.result.queueing_enabled ? 'enabled' : 'disabled'}.</p><p>{testResult.result.missing_categories.length ? `Missing categories: ${testResult.result.missing_categories.join(', ')}. These were not created by this test.` : 'Configured categories exist.'}</p>{:else}<p>Indexer capabilities and scoped feed probes passed.</p>{/if}<p>This is a connection observation, not proof that downloads or imports work.</p></div>{/if}
      {:else}<h2>Select a provider</h2><p>Review saved settings or add Torznab, Newznab or qBittorrent.</p>{/if}
    </article>
  </div>
</section>
<style>
  .heading{display:flex;justify-content:space-between;align-items:start;gap:1rem}.providers-workspace{display:grid;grid-template-columns:minmax(240px,320px) minmax(0,1fr);background:#fff;border:1px solid #cbd5de}aside{padding:1.25rem;border-right:1px solid #cbd5de}article{padding:1.5rem;min-width:0}ul{list-style:none;padding:0}li{border-bottom:1px solid #dce3e9}li button{width:100%;text-align:left;border:0;border-radius:0;padding:.8rem .4rem}li span{display:block;font-size:.85rem;color:#596b7b}fieldset{display:grid;gap:.8rem;border:1px solid #cbd5de;padding:1rem;margin:0;min-width:0}legend{font-weight:700}.check{display:flex;align-items:center;gap:.6rem}.check input{width:auto}fieldset p{margin:0;font-size:.9rem}.pagination{display:flex;gap:.5rem;margin-top:1rem}.pagination button{font-size:.8rem}.credential-warning,.delete-confirm{border-left:3px solid #a12637;padding:.8rem;background:#fff0f2}.delete-confirm{margin-top:1rem}.delete-confirm button{margin-right:.5rem}@media(max-width:800px){.providers-workspace{grid-template-columns:1fr}aside{border-right:0;border-bottom:1px solid #cbd5de}article{padding:1rem}.heading{flex-wrap:wrap}}
</style>
