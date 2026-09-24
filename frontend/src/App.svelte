<script lang="ts">
  type Series = { id:number; title:string; year?:number; path:string; poster?:string };
  type Episode = { id:number; season:number; number:number; title:string; file_path?:string };
  let series: Series[] = $state([]), selected: Series | undefined = $state(), episodes: Episode[] = $state([]), error = $state('');
  let episodeId = $state<number>(), source = $state(''), destination = $state(''), mode = $state('copy'), notice = $state('');
  async function loadSeries() { try { series = await (await fetch('/api/v1/series')).json(); } catch (e) { error = String(e); } }
  async function select(item: Series) { selected = item; episodes = await (await fetch(`/api/v1/series/${item.id}/episodes`)).json(); }
  async function preview() { if (!episodeId) return; const response = await fetch('/api/v1/imports', { method:'POST', headers:{'content-type':'application/json'}, body:JSON.stringify({episode_id:episodeId, source, destination, mode}) }); const data = await response.json(); notice = response.ok ? `Preview ${data.id} created.` : data.error; }
  loadSeries();
</script>

<svelte:head><title>Ultrasonic</title></svelte:head>
<main>
  <header><span class="mark">U</span><div><h1>Ultrasonic</h1><p>Library control</p></div></header>
  {#if error}<p class="error">{error}</p>{/if}
  <section class="layout">
    <aside><h2>Series</h2>{#each series as item}<button class:active={selected?.id === item.id} onclick={() => select(item)}>{item.title}<small>{item.year ?? ''}</small></button>{:else}<p class="muted">Import a Sonarr library to begin.</p>{/each}</aside>
    <article>{#if selected}<h2>{selected.title}</h2><p class="path">{selected.path}</p><table><thead><tr><th>Episode</th><th>Title</th><th>File</th></tr></thead><tbody>{#each episodes as episode}<tr class:selected={episodeId === episode.id} onclick={() => episodeId = episode.id}><td>S{episode.season}E{episode.number}</td><td>{episode.title}</td><td>{episode.file_path ?? 'Missing'}</td></tr>{/each}</tbody></table>{#if episodeId}<form onsubmit={(e) => { e.preventDefault(); preview(); }}><h3>Prepare import</h3><input bind:value={source} placeholder="Source file path" required /><input bind:value={destination} placeholder="Destination path" required /><select bind:value={mode}><option>copy</option><option>move</option><option>hardlink</option></select><button>Preview operation</button>{#if notice}<p class="notice">{notice}</p>{/if}</form>{/if}{:else}<div class="empty"><h2>Your library, in one place.</h2><p>Select a series to inspect episodes and prepare a safe import.</p></div>{/if}</article>
  </section>
</main>

<style>
  :global(*) { box-sizing:border-box } :global(body) { margin:0; background:#11151b; color:#e8edf2; font:15px system-ui,sans-serif } main { max-width:1180px; margin:auto; padding:32px } header { display:flex; gap:12px; align-items:center; margin-bottom:30px } .mark { display:grid; place-items:center; width:42px; height:42px; border-radius:12px; background:#8be28b; color:#101710; font-weight:800; font-size:24px } h1,h2,p { margin:0 } header p,.path,.muted { color:#8f9aa8 } header h1 { font-size:20px } header p { font-size:12px } .layout { display:grid; grid-template-columns:280px 1fr; gap:24px } aside,article { background:#1a2028; border:1px solid #2a3440; border-radius:14px; padding:20px } aside h2 { font-size:14px; text-transform:uppercase; letter-spacing:.08em; margin-bottom:12px } aside button { display:flex; width:100%; border:0; border-radius:9px; padding:11px; background:transparent; color:inherit; text-align:left; cursor:pointer; justify-content:space-between } aside button:hover,aside button.active { background:#26332b; color:#a9f0aa } small { color:#8f9aa8 } article h2 { font-size:26px } .path { margin-top:6px; font-size:13px } table { width:100%; margin-top:24px; border-collapse:collapse } th,td { padding:12px 8px; border-bottom:1px solid #2a3440; text-align:left } tr.selected { background:#26332b } th { color:#8f9aa8; font-size:12px; text-transform:uppercase } form { display:grid; gap:10px; margin-top:24px; max-width:520px } input,select,form button { border:1px solid #394654; border-radius:8px; padding:10px; background:#11151b; color:inherit } form button { background:#8be28b; color:#101710; cursor:pointer } .notice { color:#a9f0aa } .empty { min-height:360px; display:grid; place-content:center; text-align:center; gap:8px } .empty p { color:#8f9aa8 } .error { color:#ff9b9b; margin-bottom:18px }
  @media (max-width:700px) { main { padding:18px } .layout { grid-template-columns:1fr } }
</style>
