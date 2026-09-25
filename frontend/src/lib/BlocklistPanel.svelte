<script lang="ts">
  import { onMount } from 'svelte';
  import type { ApiPage, BlocklistEntry, BlocklistIdentity, BlocklistQuery, MediaDomain } from './api.generated';
  import { listBlocklist, deleteBlocklistEntry, deleteBlocklistEntries } from './api';
  let page: ApiPage<BlocklistEntry> | null = $state(null), detail: BlocklistEntry | null = $state(null);
  let media = $state(''), libraryIds = $state(''), protocol = $state(''), sort = $state('date'), direction = $state('desc');
  let query: BlocklistQuery = $state({limit:25,offset:0}), selected: BlocklistIdentity[] = $state([]);
  let confirmation: {ids:BlocklistIdentity[]; single:boolean} | null = $state(null);
  let busy = $state(false), ready = $state(false), uncertain = $state(false), error = $state(''), notice = $state('');
  let alive = true;
  const key = (id:BlocklistIdentity) => `${id.application}:${id.fingerprint}:${id.source_id}`;
  const checked = (id:BlocklistIdentity) => selected.some(item=>key(item)===key(id));
  const date = (value:string|null) => value === null ? 'Unknown' : new Date(value).toLocaleString();
  function toggle(id:BlocklistIdentity) {selected=checked(id)?selected.filter(item=>key(item)!==key(id)):[...selected,id];}
  async function load(reconcile=false) {
    if (busy || !alive) return;
    busy=true; confirmation=null; selected=[]; detail=null;
    const result=await listBlocklist(query);
    if (!alive) return;
    busy=false;
    if (result.ok) {page=result.data; ready=true; if(reconcile){uncertain=false;error='';notice='Blocklist readback complete. Inspect the current entries before another removal.';}}
    else {ready=false;error=result.error;}
  }
  async function apply() {
    if(busy || uncertain) return;
    const ids=libraryIds.trim().split(',').map(value=>value.trim()).join(',');
    if(ids && !media){error='Choose TV or Movies before filtering library IDs.';return;}
    if(ids){
      const parts=ids.split(',').map(value=>value.trim());
      if(parts.length>100 || parts.some(value=>!/^[1-9]\d*$/.test(value)||!Number.isSafeInteger(Number(value))) || new Set(parts.map(Number)).size!==parts.length){error='Enter up to 100 distinct positive library IDs, separated by commas.';return;}
    }
    query={limit:25,offset:0,...(media?{media_type:media as MediaDomain}:{}),...(ids?media==='tv'?{series_ids:ids}:{movie_ids:ids}:{}),...(protocol?{protocols:protocol}:{}),sort:sort as BlocklistQuery['sort'],sort_direction:direction as BlocklistQuery['sort_direction']};
    error='';notice='';await load();
  }
  async function remove() {
    if(!confirmation || busy || uncertain || !ready) return;
    const request=confirmation;busy=true;error='';notice='';
    const result=request.single?await deleteBlocklistEntry(request.ids[0]):await deleteBlocklistEntries({ids:request.ids});
    if(!alive)return;
    busy=false;confirmation=null;
    if(result.ok){notice='Blocklist entries removed. Library records, files and client downloads remain intact.';await load();}
    else {error=`${result.error}${result.code?` (${result.code})`:''}`;uncertain=!result.status;if(uncertain)error+=' The response was lost or unreadable; removal may have completed. Refresh blocklist to read back saved state. No automatic removal retry will occur.';}
  }
  onMount(()=>{alive=true;void load();return()=>{alive=false;};});
</script>
<section aria-labelledby="blocklist-heading">
  <div class="heading"><div><h1 id="blocklist-heading">Blocklist</h1><p>Review blocklist records imported from Sonarr and Radarr snapshots. This view does not create entries or enforce failed-download decisions.</p></div><button disabled={busy} onclick={()=>load(true)}>Refresh blocklist</button></div>
  {#if busy}<p role="status">Reading or updating blocklist…</p>{/if}{#if error}<p role="alert" class="error">{error}</p>{/if}{#if notice}<p role="status">{notice}</p>{/if}
  <form class="filters" onsubmit={(event)=>{event.preventDefault();void apply();}}><fieldset disabled={busy || uncertain || !!confirmation}><legend>Blocklist filters</legend><label>Blocklist media<select bind:value={media}><option value="">All media</option><option value="tv">TV</option><option value="movies">Movies</option></select></label><label>Library IDs<input bind:value={libraryIds} placeholder="Series IDs for TV, movie IDs for Movies" /></label><label>Blocklist protocol<select bind:value={protocol}><option value="">All protocols, including absent</option><option value="torrent">Torrent</option><option value="usenet">Usenet</option><option value="unknown">Explicitly unknown</option></select></label><label>Sort blocklist by<select bind:value={sort}><option value="date">Date</option><option value="source_title">Source title</option></select></label><label>Blocklist sort direction<select bind:value={direction}><option value="desc">Descending</option><option value="asc">Ascending</option></select></label><button>Apply blocklist filters</button></fieldset></form>
  <div class="surface">
    {#if page}<p>{page.total} matching entries. Selection applies only to the current page.</p><button disabled={busy || uncertain || !ready || selected.length===0} onclick={()=>confirmation={ids:[...selected],single:false}}>Remove selected entries ({selected.length})</button><div class="scroll"><table><thead><tr><th scope="col">Select</th><th scope="col">Source title</th><th scope="col">Library target</th><th scope="col">Date</th><th scope="col">Protocol</th></tr></thead><tbody>{#each page.items as entry (key(entry.id))}<tr><td><input type="checkbox" aria-label={`Select ${entry.target.media_type} ${entry.source_title} (${entry.id.source_id})`} checked={checked(entry.id)} disabled={busy || uncertain || !!confirmation} onchange={()=>toggle(entry.id)} /></td><td><button disabled={busy || !!confirmation} onclick={()=>detail=entry}>{entry.source_title}</button></td><td>{#if entry.target.media_type==='tv'}TV series {entry.target.series_id}<small>Episodes: {entry.target.episode_ids.join(', ') || 'None recorded'}</small>{:else}Movie {entry.target.movie_id}{/if}</td><td>{date(entry.occurred_at)}</td><td>{entry.protocol??'Not supplied'}</td></tr>{:else}<tr><td colspan="5">No matching imported blocklist entries.</td></tr>{/each}</tbody></table></div><div class="actions"><button disabled={busy || uncertain || !!confirmation || page.offset===0} onclick={()=>{query={...query,offset:Math.max(0,page!.offset-25)};void load();}}>Previous blocklist page</button><button disabled={busy || uncertain || !!confirmation || page.offset+page.limit>=page.total} onclick={()=>{query={...query,offset:page!.offset+page!.limit};void load();}}>Next blocklist page</button></div>{/if}
    {#if detail}<section aria-label="Blocklist entry detail"><h2>{detail.source_title}</h2><dl><dt>Origin</dt><dd>{detail.origin}</dd><dt>Snapshot application</dt><dd>{detail.id.application}</dd><dt>Snapshot fingerprint</dt><dd>{detail.id.fingerprint}</dd><dt>Source record ID</dt><dd>{detail.id.source_id}</dd><dt>Library target</dt><dd>{detail.target.media_type==='tv'?`TV series ${detail.target.series_id}; episodes ${detail.target.episode_ids.join(', ')}`:`Movie ${detail.target.movie_id}`}</dd><dt>Date</dt><dd>{date(detail.occurred_at)}</dd><dt>Published</dt><dd>{date(detail.published_at)}</dd><dt>Protocol</dt><dd>{detail.protocol??'Not supplied'}</dd><dt>Size (bytes)</dt><dd>{detail.size_bytes??'Unknown'}</dd><dt>Quality ID</dt><dd>{detail.quality?.quality_id??'Unknown'}</dd><dt>Quality revision</dt><dd>{detail.quality?.revision ? `Version ${detail.quality.revision.version}, real ${detail.quality.revision.real}, repack ${detail.quality.revision.is_repack ? 'yes' : 'no'}` : 'Unknown'}</dd><dt>Language IDs</dt><dd>{detail.languages===null?'Unknown':detail.languages.join(', ')||'None recorded'}</dd></dl><button disabled={busy || uncertain || !ready} onclick={()=>confirmation={ids:[detail!.id],single:true}}>Remove this entry</button></section>{/if}
    {#if confirmation}<section class="confirmation" aria-label="Confirm blocklist removal"><h2>Remove {confirmation.ids.length} blocklist {confirmation.ids.length===1?'entry':'entries'}?</h2><p>This removes only the selected imported blocklist records and preserves their removal on replay of the same snapshot. It does not delete files, library targets or client downloads.</p><ul>{#each confirmation.ids as id (key(id))}<li>{id.application} source record {id.source_id}<small>{id.fingerprint}</small></li>{/each}</ul><div class="actions"><button disabled={busy || uncertain} onclick={remove}>Confirm blocklist removal</button><button disabled={busy} onclick={()=>confirmation=null}>Keep blocklist entries</button></div></section>{/if}
  </div>
</section>
<style>
  .heading{display:flex;justify-content:space-between;align-items:start;gap:1rem}.filters{max-width:none}fieldset{display:flex;gap:1rem;flex-wrap:wrap;border:1px solid #cbd5de;padding:1rem;align-items:end}fieldset label{flex:1;min-width:150px}legend{font-weight:700}.surface{background:#fff;border:1px solid #cbd5de;padding:1.25rem}.scroll{overflow:auto}table{width:100%;border-collapse:collapse;text-align:left}th,td{padding:.8rem .5rem;border-bottom:1px solid #dce3e9;vertical-align:top}td input{width:auto}small{display:block;overflow-wrap:anywhere;color:#596b7b}.actions{margin-top:1rem}.confirmation{padding:1rem;border-left:3px solid #a12637;background:#fff0f2;margin-top:1rem}@media(max-width:700px){.heading{flex-wrap:wrap}.surface{padding:1rem}}
</style>
