<script lang="ts">
  import { onMount } from 'svelte';
  import type { QualityProfilePage, Episode, LibraryItem, LibraryPage, LibraryPatch, LookupResult, MediaDomain, MediaTarget, SeriesType, MinimumAvailability } from './lib/api.generated';
  import { listQualityProfiles, listLibrary, getLibrary, lookupLibrary, addLibrary, updateLibrary, listEpisodes, monitorEpisode } from './lib/api';
  import RssPanel from './lib/RssPanel.svelte';
  import ReleaseSearchPanel from './lib/ReleaseSearchPanel.svelte';
  import ImportPanel from './lib/ImportPanel.svelte';
  import ProviderPanel from './lib/ProviderPanel.svelte';
  import ActivityPanel from './lib/ActivityPanel.svelte';
  import BlocklistPanel from './lib/BlocklistPanel.svelte';
  import MetadataRefreshPanel from './lib/MetadataRefreshPanel.svelte';
  let searchTarget: MediaTarget | null = $state(null);
  let view = $state<'library' | 'providers' | 'activity' | 'blocklist' | 'rss'>('library');
  let domain: MediaDomain = $state('tv'), page: LibraryPage | null = $state(null), selected: LibraryItem | null = $state(null);
  let episodes: Episode[] = $state([]), episodeTotal = $state(0), episodeOffset = $state(0), episodeId: number | null = $state(null);
  let loading = $state(false), detailLoading = $state(false), saving = $state(false), error = $state(''), notice = $state('');
  let adding = $state(false), term = $state(''), results: LookupResult[] = $state([]), choice: LookupResult | null = $state(null), path = $state(''), searching = $state(false), searched = $state(false);
  let seriesType = $state(''), seasonFolder = $state(''), sceneNumbering = $state(''), newItems = $state(''), availability = $state('');
  let profiles: QualityProfilePage | null = $state(null), profileId: number | null = $state(null), profilesLoading = $state(false), profilesError = $state('');
  let profileVersion = 0;
  let listVersion = 0, detailVersion = 0, searchVersion = 0, alive = true;
  const target: MediaTarget | null = $derived.by(() => selected ? domain === 'movies' ? selected.statistics.file_count === 0 ? {media_type:'movie',id:selected.id} : null : episodeId !== null && episodes.some(e => e.id === episodeId && !e.has_file) ? {media_type:'episode',id:episodeId} : null : null);
  const targetLabel = $derived.by(() => selected ? domain === 'movies' ? selected.title : `${selected.title}: ${episodes.find(e => e.id === episodeId)?.title ?? ''}` : '');
  function fields(item: LibraryItem) { profileId = item.settings.quality_profile_id; seriesType = item.settings.series_type ?? ''; seasonFolder = item.settings.season_folder === null ? '' : String(item.settings.season_folder); sceneNumbering = item.settings.use_scene_numbering === null ? '' : String(item.settings.use_scene_numbering); newItems = item.settings.monitor_new_items ?? ''; availability = item.settings.minimum_availability ?? ''; }
  async function load(offset = 0) {
    const version = ++listVersion, scope = domain; loading = true; error = '';
    const result = await listLibrary(scope, offset);
    if (!alive || version !== listVersion) return;
    loading = false; if (result.ok) page = result.data; else error = result.error;
  }
  async function loadProfiles(offset = 0) {
    const version = ++profileVersion, scope = domain; profilesLoading = true; profilesError = '';
    const result = await listQualityProfiles(scope, offset);
    if (!alive || version !== profileVersion || scope !== domain) return;
    profilesLoading = false;
    if (result.ok) profiles = result.data; else profilesError = result.error;
  }
  function changeDomain(next: MediaDomain) {
    searchTarget = null; domain = next; ++profileVersion; profiles = null; profilesError = ''; profilesLoading = false; ++detailVersion; ++searchVersion; selected = null; episodes = []; episodeId = null; page = null; adding = false; results = []; choice = null; searching = false; searched = false; detailLoading = false; notice = ''; error = ''; void load();
  }
  async function select(id: number, offset = 0) {
    searchTarget = null; const version = ++detailVersion, scope = domain; detailLoading = true; error = ''; episodeId = null; selected = null; episodes = [];
    const result = await getLibrary(scope, id);
    if (!alive || version !== detailVersion) return;
    if (!result.ok) { detailLoading = false; selected = null; error = result.error; return; }
    selected = result.data; fields(selected); if (!profiles && !profilesLoading) void loadProfiles(); episodes = []; episodeOffset = offset;
    if (scope === 'tv') {
      const rows = await listEpisodes(id, offset);
      if (!alive || version !== detailVersion) return;
      if (rows.ok) {episodes = rows.data.items; episodeTotal = rows.data.total;} else error = rows.error;
    }
    detailLoading = false;
  }
  async function search() {
    const version = ++searchVersion, scope = domain; searching = true; searched = false; results = []; choice = null; error = '';
    const result = await lookupLibrary(scope, term);
    if (!alive || version !== searchVersion) return;
    searching = false; searched = true;
    if (result.ok) results = result.data.filter(item => item.media_type === scope); else error = result.error;
  }
  async function add() {
    if (!choice || saving) return;
    const scope = domain, externalId = choice.external_id, version = searchVersion; saving = true; error = '';
    const result = await addLibrary(scope, externalId, path);
    saving = false;
    if (!alive || scope !== domain || version !== searchVersion) return;
    if (result.ok) { adding = false; choice = null; notice = 'Added to library.'; await load(); if (alive && scope === domain && version === searchVersion) await select(result.data.id); }
    else { error = `${result.error} If the response was lost, refresh the library before trying again.`; }
  }
  async function patch(patch: LibraryPatch) {
    if (!selected || saving) return;
    const item = selected, version = detailVersion; saving = true; error = ''; notice = '';
    const result = await updateLibrary(item.media_type, item.id, patch); saving = false;
    if (!alive || version !== detailVersion) return;
    if (!result.ok) {error = result.error; return;}
    notice = 'Changes saved.'; await select(item.id, episodeOffset); void load(page?.offset ?? 0);
  }
  async function monitor(episode: Episode) {
    if (saving) return;
    const version = detailVersion; saving = true; error = '';
    const result = await monitorEpisode(episode.id, !episode.monitored); saving = false;
    if (!alive || version !== detailVersion) return;
    if (result.ok) { episodes = episodes.map(item => item.id === episode.id ? result.data : item); notice = 'Episode monitoring saved.'; }
    else error = result.error;
  }
  function saveSettings() {
    void patch(domain === 'tv' ? {quality_profile_id: profileId, series_type: (seriesType || null) as SeriesType | null, season_folder: seasonFolder === '' ? null : seasonFolder === 'true', use_scene_numbering: sceneNumbering === '' ? null : sceneNumbering === 'true', monitor_new_items: (newItems || null) as 'all' | 'none' | null} : {quality_profile_id: profileId, minimum_availability: (availability || null) as MinimumAvailability | null});
  }
  function refreshAfterImport() { void load(page?.offset ?? 0); if (selected) void select(selected.id, episodeOffset); }
  onMount(() => { alive = true; void load(); return () => {alive = false; ++listVersion; ++detailVersion; ++searchVersion; ++profileVersion;}; });
</script>

<svelte:head><title>hrrdarr · Library</title></svelte:head>
<header class="masthead"><a href="#library" class="brand">hrrdarr</a><span>Media library</span></header>
<main id="library">
  <nav class="app-nav" aria-label="Workspace"><button aria-pressed={view === 'library'} onclick={() => view = 'library'}>Library</button><button aria-pressed={view === 'providers'} onclick={() => view = 'providers'}>Providers</button><button aria-pressed={view === 'activity'} onclick={() => view = 'activity'}>Activity</button><button aria-pressed={view === 'blocklist'} onclick={() => view = 'blocklist'}>Blocklist</button><button aria-pressed={view === 'rss'} onclick={() => view = 'rss'}>RSS</button></nav>
  {#if view === 'rss'}<RssPanel />{/if}
  {#if view === 'providers'}<ProviderPanel />{/if}
  {#if view === 'activity'}<ActivityPanel />{/if}
  {#if view === 'blocklist'}<BlocklistPanel />{/if}
  <div hidden={view !== 'library'}>
  <div class="toolbar"><nav aria-label="Library type"><button aria-pressed={domain === 'tv'} onclick={() => changeDomain('tv')}>TV</button><button aria-pressed={domain === 'movies'} onclick={() => changeDomain('movies')}>Movies</button></nav><button class="primary" onclick={() => {adding = !adding; ++searchVersion; searching = false; results = []; choice = null; searched = false;}}>{adding ? 'Close add form' : domain === 'tv' ? 'Add series' : 'Add movie'}</button></div>
  {#if error}<p role="alert" class="error">{error}</p>{/if}{#if notice}<p role="status">{notice}</p>{/if}
  {#if adding}
    <section class="add-section" aria-labelledby="add-heading"><h1 id="add-heading">{domain === 'tv' ? 'Add series' : 'Add movie'}</h1>
      <form onsubmit={(event) => {event.preventDefault(); void search();}}><label>Search catalogue<input bind:value={term} required maxlength="256" placeholder={domain === 'tv' ? 'Title or tvdb:123' : 'Title, tmdb:123 or imdb:tt1234567'} /></label><button disabled={searching}>Search</button></form>
      {#if searching}<p role="status">Searching catalogue…</p>{:else if searched && !results.length}<p>No matches. Try a different title or catalogue ID.</p>{/if}
      <ul class="lookup-results">{#each results as result (`${result.media_type}:${result.external_id}`)}<li><button aria-pressed={choice?.external_id === result.external_id} onclick={() => {choice = result; path = '';}}>{result.title} {result.year ?? ''}<small>{domain === 'tv' ? 'TVDB' : 'TMDB'} {result.external_id}</small></button></li>{/each}</ul>
      {#if choice}<form onsubmit={(event) => {event.preventDefault(); void add();}}><h2>{choice.title}</h2><label>Existing library directory<input bind:value={path} required placeholder="/library/title" /></label><p>Choose a directory that already exists. Adding does not create folders, search for downloads or import files. Monitoring starts enabled; edit it below after adding.</p><button class="primary" disabled={saving}>Add to library</button></form>{/if}
    </section>
  {/if}
  <div class="workspace">
    <aside aria-label="Library collection"><div class="section-heading"><h1>{domain === 'tv' ? 'Series' : 'Movies'}</h1><button disabled={loading} onclick={() => load(page?.offset ?? 0)}>Refresh library</button></div>
      {#if loading}<p role="status">Loading library…</p>{/if}
      {#if page}<p class="muted">{page.total} in library</p><ul class="collection">{#each page.items as item (`${item.media_type}:${item.id}`)}<li><button aria-current={selected?.id === item.id ? 'true' : undefined} onclick={() => {adding = false; void select(item.id);}}><span>{item.title}<small>{item.year ?? 'Year unknown'}</small></span><span class="count">{item.statistics.file_count} files</span></button></li>{:else}<li class="empty">No {domain === 'tv' ? 'series' : 'movies'} yet. Use {domain === 'tv' ? 'Add series' : 'Add movie'} to search the catalogue.</li>{/each}</ul>
        <div class="pagination"><button disabled={loading || page.offset === 0} onclick={() => load(Math.max(0, page!.offset - 25))}>Previous library page</button><button disabled={loading || page.offset + page.limit >= page.total} onclick={() => load(page!.offset + page!.limit)}>Next library page</button></div>
      {/if}
    </aside>
    <article aria-label="Library detail">
      {#if detailLoading}<p role="status">Loading details…</p>{/if}
      {#if selected && !detailLoading}
        <h1>{selected.title}</h1><p class="path">{selected.path}</p><p>{selected.statistics.file_count} associated files · {selected.monitored ? 'Monitored' : 'Unmonitored'}</p>
        <button disabled={saving} onclick={() => patch({monitored: !selected!.monitored})}>{selected.monitored ? 'Unmonitor' : 'Monitor'} {domain === 'tv' ? 'series' : 'movie'}</button>
        {#if view === 'library'}{#key `${selected.media_type}:${selected.id}`}<MetadataRefreshPanel target={selected.media_type === 'tv' ? {media_type:'tv',series_id:selected.id} : {media_type:'movies',movie_id:selected.id}} onreload={() => {if(selected) {void load(page?.offset ?? 0); void select(selected.id,episodeOffset);}}} />{/key}{/if}
        <details><summary>Library settings</summary><form onsubmit={(event) => {event.preventDefault(); saveSettings();}}>
          {#if domain === 'tv'}<label>Series type<select aria-label="Series type" bind:value={seriesType}><option value="">Unknown</option><option value="standard">Standard</option><option value="daily">Daily</option><option value="anime">Anime</option></select></label><label>Season folders<select aria-label="Season folders" bind:value={seasonFolder}><option value="">Unknown</option><option value="true">Enabled</option><option value="false">Disabled</option></select></label><label>Scene numbering<select aria-label="Scene numbering" bind:value={sceneNumbering}><option value="">Unknown</option><option value="true">Enabled</option><option value="false">Disabled</option></select></label><label>Monitor new seasons<select aria-label="Monitor new seasons" bind:value={newItems}><option value="">Unknown</option><option value="all">All</option><option value="none">None</option></select></label>
          {:else}<label>Minimum availability<select aria-label="Minimum availability" bind:value={availability}><option value="">Unknown</option><option value="tba">To be announced</option><option value="announced">Announced</option><option value="in_cinemas">In cinemas</option><option value="released">Released</option></select></label><p>Background release decisions use this policy and the known release dates.</p>{/if}
          <label>Quality profile<select aria-label="Quality profile" bind:value={profileId} disabled={profilesLoading}>
            <option value={null}>Unassigned</option>
            {#if profileId !== null && !profiles?.items.some(item => item.id === profileId)}<option value={profileId}>{profileId === selected.settings.quality_profile_id ? selected.settings.profile_name ?? `Profile ${profileId}` : `Profile ${profileId}`}</option>{/if}
            {#each profiles?.items ?? [] as profile (profile.id)}<option value={profile.id}>{profile.name}</option>{/each}
          </select></label>
          {#if profilesLoading}<p role="status">Loading quality profiles…</p>{/if}
          {#if profilesError}<p role="alert" class="error">{profilesError}</p><button type="button" onclick={() => loadProfiles()}>Retry quality profiles</button>{/if}
          {#if profiles}<div class="actions"><button type="button" disabled={profilesLoading || profiles.offset === 0} onclick={() => loadProfiles(Math.max(0, profiles!.offset - 50))}>Previous profiles</button><button type="button" disabled={profilesLoading || profiles.offset + profiles.limit >= profiles.total} onclick={() => loadProfiles(profiles!.offset + profiles!.limit)}>Next profiles</button></div>{/if}
          <button disabled={saving || profilesLoading}>Save settings</button></form></details>
        {#if domain === 'tv'}
          <h2>Seasons</h2><p class="muted">Changing a season updates its episodes. Series monitoring leaves individual flags intact.</p><div class="seasons">{#each selected.seasons ?? [] as season (season.number)}<button disabled={saving} aria-pressed={season.monitored} onclick={() => patch({seasons:[{number:season.number,monitored:!season.monitored}]})}>{season.number === 0 ? 'Specials' : `Season ${season.number}`}: {season.monitored ? 'monitored' : 'unmonitored'}</button>{/each}</div>
          <h2>Episodes</h2><div class="table-scroll"><table><thead><tr><th scope="col">Episode</th><th scope="col">Monitoring</th><th scope="col">File / import</th></tr></thead><tbody>{#each episodes as episode (episode.id)}<tr><td><strong>S{episode.season} E{episode.number}</strong><br />{episode.title}</td><td><button disabled={saving} aria-pressed={episode.monitored} onclick={() => monitor(episode)}>{episode.monitored ? 'Unmonitor' : 'Monitor'} {episode.title}</button></td><td><button onclick={() => searchTarget = {media_type:'episode',id:episode.id}}>Search releases for {episode.title}</button>{#if episode.has_file}<span class="path">{episode.file_path ?? 'File associated'}</span>{:else}<button aria-pressed={episodeId === episode.id} onclick={() => episodeId = episode.id}>Import {episode.title}</button>{/if}</td></tr>{:else}<tr><td colspan="3">No episodes in this catalogue.</td></tr>{/each}</tbody></table></div>
          <div class="pagination"><button disabled={episodeOffset === 0} onclick={() => select(selected!.id, Math.max(0, episodeOffset - 25))}>Previous episodes</button><span>{episodeTotal} episodes</span><button disabled={episodeOffset + 25 >= episodeTotal} onclick={() => select(selected!.id, episodeOffset + 25)}>Next episodes</button></div>
        {:else if selected.statistics.file_count > 0}<p>This movie already has a file association. Initial import will not replace it.</p>{/if}
        {#if domain === 'movies'}<button onclick={() => searchTarget = {media_type:'movie',id:selected!.id}}>Search movie releases</button>{/if}
        {#if searchTarget && view === 'library'}{#key `${searchTarget.media_type}:${searchTarget.id}`}<ReleaseSearchPanel target={searchTarget} />{/key}{/if}
      {:else if !detailLoading}<div class="empty"><h1>Select a title</h1><p>Review monitoring, inspect episodes and import an existing media file.</p></div>{/if}
    </article>
  </div>
  </div>
  {#if view === 'library'}<ImportPanel {target} label={targetLabel} oncomplete={refreshAfterImport} />{/if}
</main>

<style>
  :global(*){box-sizing:border-box} :global(body){margin:0;background:#f3f6f8;color:#243746;font:15px/1.5 Inter,"Segoe UI",sans-serif} :global(button),:global(input),:global(select){font:inherit} :global(button){border:1px solid #98a9b8;border-radius:5px;padding:.5rem .8rem;background:#fff;color:#243746;cursor:pointer} :global(button:hover:not(:disabled)){background:#e7eef7} :global(button:disabled){opacity:.55;cursor:default} :global(button[aria-pressed=true]),:global(button[aria-current=true]){background:#e0ebfb;border-color:#245ba8;color:#173f79} :global(:focus-visible){outline:3px solid #245ba8;outline-offset:3px} :global(.primary){background:#245ba8;color:#fff;border-color:#245ba8} :global(.primary:hover:not(:disabled)){background:#194a8e} :global(h1){font-size:1.65rem;line-height:1.2;margin:0 0 1rem} :global(h2){font-size:1.15rem;margin:1.5rem 0 .6rem} :global(h3){font-size:1rem} :global(p){max-width:76ch} :global(.error){color:#a12637;background:#fff0f2;padding:.8rem;border-left:3px solid #a12637} :global(.muted),small{color:#596b7b} :global(.path){overflow-wrap:anywhere} :global(form){display:grid;gap:.8rem;max-width:620px;margin:1rem 0} :global(label){display:grid;gap:.3rem;font-weight:600} :global(input),:global(select){width:100%;padding:.6rem;border:1px solid #8e9faa;border-radius:4px;background:#fff;color:#243746} :global(form button){justify-self:start} :global(.actions){display:flex;flex-wrap:wrap;gap:.6rem} :global(.import-panel){background:#fff;border-top:3px solid #245ba8;padding:1.5rem;margin-top:1.5rem} :global(.import-panel h2){margin-top:0} :global(dl){display:grid;grid-template-columns:130px 1fr;gap:.4rem} :global(dt){font-weight:600} :global(dd){margin:0;overflow-wrap:anywhere}
  .app-nav{margin-bottom:1.25rem;flex-wrap:wrap}
  .masthead{padding:1rem max(1.25rem,calc((100vw - 1360px)/2));display:flex;align-items:center;gap:1.5rem;background:#243746;color:white}.brand{font-size:1.5rem;letter-spacing:-.06em;font-weight:800;color:inherit;text-decoration:none}main{max-width:1400px;padding:1.5rem;margin:auto}.toolbar{display:flex;justify-content:space-between;gap:1rem;margin-bottom:1.5rem}nav{display:flex;gap:.5rem}.workspace{display:grid;grid-template-columns:minmax(260px,330px) minmax(0,1fr);background:#fff;border:1px solid #cbd5de}aside{border-right:1px solid #cbd5de;padding:1.25rem}article{padding:1.5rem;min-width:0}.section-heading{display:flex;align-items:start;justify-content:space-between;gap:1rem}.section-heading h1{font-size:1.25rem}.section-heading button{font-size:.8rem}.collection,.lookup-results{list-style:none;margin:0;padding:0}.collection li{border-bottom:1px solid #dce3e9}.collection button{width:100%;display:flex;align-items:center;justify-content:space-between;text-align:left;border:0;border-radius:0;gap:.8rem;padding:1rem .5rem}small{display:block;font-size:.8rem}.count{white-space:nowrap;font-size:.8rem}.pagination{display:flex;align-items:center;justify-content:space-between;gap:.5rem;margin-top:1rem}.pagination button{font-size:.8rem}.empty{padding:2rem 0}.add-section{background:#fff;padding:1.5rem;border-left:4px solid #245ba8;margin-bottom:1.5rem}.lookup-results{max-width:620px}.lookup-results button{width:100%;text-align:left;margin-bottom:.4rem}.seasons{display:flex;flex-wrap:wrap;gap:.5rem}details{margin:1.5rem 0}summary{cursor:pointer;font-weight:600}.table-scroll{overflow-x:auto}table{border-collapse:collapse;width:100%;text-align:left}th,td{padding:.8rem .5rem;border-bottom:1px solid #dce3e9;vertical-align:top}th{font-size:.85rem}td button{font-size:.85rem}
  @media(max-width:800px){.workspace{grid-template-columns:1fr}aside{border-right:0;border-bottom:1px solid #cbd5de}main{padding:1rem}article{padding:1rem}.masthead{padding:1rem}.toolbar{align-items:center}:global(dl){grid-template-columns:1fr}:global(dd){margin-bottom:.5rem}.pagination{flex-wrap:wrap}}
</style>
