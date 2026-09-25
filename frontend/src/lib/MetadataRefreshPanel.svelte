<script lang="ts">
  import { onMount } from 'svelte';
  import type { ApiPage, MetadataCommand, MetadataRefreshTarget } from './api.generated';
  import { listMetadataCommands, getMetadataCommand, createMetadataCommand, cancelMetadataCommand, deleteMetadataCommand } from './api';
  let {target, onreload}: {target:MetadataRefreshTarget; onreload:()=>void} = $props();
  let page: ApiPage<MetadataCommand> | null = $state(null), selected: MetadataCommand | null = $state(null);
  let offset = $state(0), busy = $state(false), ready = $state(false), uncertain = $state(false), confirming = $state(false), error = $state(''), notice = $state('');
  let alive = true, timer: ReturnType<typeof setTimeout> | undefined;
  const terminal = (command:MetadataCommand) => ['succeeded','failed','cancelled'].includes(command.status);
  const date = (value:number|null) => value === null ? 'Not recorded' : new Date(value*1000).toLocaleString();
  const matches = (command:MetadataCommand) => command.target.media_type === target.media_type && (command.target.media_type === 'tv' && target.media_type === 'tv' ? command.target.series_id === target.series_id : command.target.media_type === 'movies' && target.media_type === 'movies' && command.target.movie_id === target.movie_id);
  function pollLater() {
    clearTimeout(timer);
    if (alive && !document.hidden) timer = setTimeout(() => void read(),5000);
  }
  async function read(explicit = false) {
    if (!alive || busy || document.hidden) return;
    busy = true; clearTimeout(timer);
    const query = target.media_type === 'tv' ? {media_type:target.media_type,series_id:target.series_id,limit:25,offset} : {media_type:target.media_type,movie_id:target.movie_id,limit:25,offset};
    const result = await listMetadataCommands(query);
    if (!alive) return;
    let success = result.ok;
    if (result.ok && result.data.items.every(matches)) {
      page = result.data;
      if (!selected && result.data.items.length) selected = result.data.items[0];
    } else {
      success = false; ready = false; error = result.ok ? 'Metadata history did not match this library target.' : result.error;
    }
    if (success && selected && !document.hidden) {
      const detail = await getMetadataCommand(selected.id);
      if (!alive) return;
      if (detail.ok && matches(detail.data)) selected = detail.data;
      else if (!detail.ok && detail.status === 404) {selected = null; confirming = false;}
      else {success = false; ready = false; error = detail.ok ? 'Metadata command target mismatch.' : detail.error;}
    }
    if (success) {
      ready = true;
      if (explicit) {uncertain = false; error = ''; notice = 'Metadata command status checked. Inspect the recorded outcome before requesting another refresh.';}
    }
    busy = false; pollLater();
  }
  async function mutate(action:'refresh'|'cancel'|'delete') {
    if (!ready || busy || uncertain || (action !== 'refresh' && !selected)) return;
    busy = true; clearTimeout(timer); error = ''; notice = ''; confirming = false;
    const result = action === 'refresh' ? await createMetadataCommand({target,priority:'normal'}) : action === 'cancel' ? await cancelMetadataCommand(selected!.id) : await deleteMetadataCommand(selected!.id);
    if (!alive) return;
    if (result.ok) {
      if (action === 'delete') selected = null;
      else if (result.data && matches(result.data)) selected = result.data;
      else {error = 'Metadata command target mismatch.'; ready = false;}
      notice = action === 'refresh' ? 'Metadata refresh requested.' : action === 'cancel' ? 'Cancellation requested for this metadata command.' : 'Metadata command history deleted. Library records and files remain.';
    } else {
      error = `${result.error}${result.code ? ` (${result.code})` : ''}`;
      uncertain = !result.status;
      if (uncertain) {offset = 0; error += ' The request may have committed. Check metadata status before another action; no request will be retried automatically.';}
    }
    busy = false; await read();
  }
  onMount(() => {
    alive = true; void read();
    const visibility = () => {clearTimeout(timer); if (!document.hidden) void read();};
    document.addEventListener('visibilitychange',visibility);
    return () => {alive = false; clearTimeout(timer); document.removeEventListener('visibilitychange',visibility);};
  });
</script>
<section class="metadata-refresh" aria-label="Metadata refresh">
  <h2>Metadata refresh</h2>
  <p>Refresh this {target.media_type === 'tv' ? 'series' : 'movie'} from its catalogue identity. This does not scan media, download artwork or search for releases.</p>
  <div class="actions"><button disabled={!ready || busy || uncertain} onclick={() => mutate('refresh')}>Refresh metadata</button><button disabled={busy} onclick={() => read(true)}>Check metadata status</button></div>
  {#if busy}<p role="status">Checking metadata refresh…</p>{/if}{#if error}<p role="alert" class="error">{error}</p>{/if}{#if notice}<p role="status">{notice}</p>{/if}
  {#if page}<p>{page.total} metadata commands for this title.</p><ul>{#each page.items as command (command.id)}<li><button disabled={busy} aria-current={selected?.id === command.id ? 'true' : undefined} onclick={() => {selected = command; confirming = false; void read();}}>{command.name}: {command.status} ({command.id})</button></li>{/each}</ul><div class="actions"><button disabled={busy || offset===0} onclick={() => {offset=Math.max(0,offset-25); void read();}}>Previous metadata commands</button><button disabled={busy || offset+25>=page.total} onclick={() => {offset+=25; void read();}}>Next metadata commands</button></div>{/if}
  {#if selected}
    <div aria-label="Metadata command detail"><h3>{selected.name}: {selected.status}</h3><dl><dt>Command ID</dt><dd>{selected.id}</dd><dt>Library target</dt><dd>{selected.target.media_type} {selected.target.media_type === 'tv' ? selected.target.series_id : selected.target.movie_id}</dd><dt>Catalogue ID</dt><dd>{selected.external_id}</dd><dt>Attempts / priority</dt><dd>{selected.attempts} / {selected.priority}</dd><dt>Created</dt><dd>{date(selected.created_at)}</dd><dt>Started</dt><dd>{date(selected.started_at)}</dd><dt>Completed</dt><dd>{date(selected.completed_at)}</dd><dt>Next attempt</dt><dd>{terminal(selected) ? 'No further attempt' : date(selected.next_attempt_at)}</dd><dt>Error</dt><dd>{selected.error_code ?? 'No recorded error'}</dd><dt>Records updated</dt><dd>{selected.status === 'succeeded' ? selected.records_updated : 'No successful result from this command'}</dd></dl>
      {#if selected.status === 'succeeded'}<p>Metadata refresh completed. Reload library details to view the current catalogue facts. Reloading discards unsaved library settings.</p><button disabled={busy} onclick={onreload}>Reload library details</button>{/if}
      {#if terminal(selected)}<button disabled={busy || uncertain || !ready} onclick={() => confirming=true}>Delete metadata command history</button>{:else}<button disabled={busy || uncertain || !ready} onclick={() => mutate('cancel')}>Cancel metadata refresh</button>{/if}
    </div>
  {/if}
  {#if confirming}<p>Delete only this terminal command history? Library records and files remain intact.</p><div class="actions"><button disabled={busy || uncertain} onclick={() => mutate('delete')}>Confirm delete metadata history</button><button onclick={() => confirming=false}>Keep metadata history</button></div>{/if}
</section>
<style>
  .metadata-refresh{border-top:1px solid #cbd5de;border-bottom:1px solid #cbd5de;padding:1rem 0;margin:1.5rem 0}.metadata-refresh h2{margin-top:0}ul{list-style:none;padding:0;max-height:15rem;overflow:auto}li{margin:.35rem 0}li button{width:100%;text-align:left;overflow-wrap:anywhere}dd{overflow-wrap:anywhere}.actions{margin:.75rem 0}
</style>
