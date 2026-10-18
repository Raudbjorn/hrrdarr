<script lang="ts">
  import {onMount} from 'svelte';
  import {getHistory} from './api';
  import type {HistoryQuery} from './api.generated';
  import {buildHistoryQuery,describeHistoryFailure,describeHistoryPage,emptyHistoryForm,HISTORY_DEFAULT_LIMIT,HISTORY_MAX_LIMIT,HISTORY_MAX_OFFSET,HISTORY_NOTES,type HistoryErrorView,type HistoryPageView} from './history-view';
  let form=$state(emptyHistoryForm()),formErrors=$state<string[]>([]);
  let applied=$state<HistoryQuery|null>(null),page=$state<HistoryPageView|null>(null),failure=$state<HistoryErrorView|null>(null),loading=$state(false);
  let alive=true,epoch=0;
  async function run(query:HistoryQuery){
    const version=++epoch;
    loading=true;failure=null;
    const result=await getHistory(query);
    if(!alive||version!==epoch)return;
    loading=false;
    if(!result.ok){failure=describeHistoryFailure(result);return;}
    const described=describeHistoryPage(result.data);
    if(described.kind==='error'){page=null;failure={code:'invalid_response',status:null,title:'Unrecognised response',advice:'The response did not match the history contract and was not displayed.',retryable:true,message:described.message};return;}
    applied=query;page=described.view;
  }
  function apply(event:SubmitEvent){
    event.preventDefault();
    const built=buildHistoryQuery(form);
    if(!built.ok){formErrors=built.errors;return;}
    formErrors=[];
    form.limit=String(built.query.limit);form.offset=String(built.query.offset);
    void run(built.query);
  }
  function reset(){form=emptyHistoryForm();formErrors=[];void run({limit:HISTORY_DEFAULT_LIMIT,offset:0});}
  function go(offset:number|null){if(offset===null||!applied||loading)return;form.offset=String(offset);void run({...applied,offset});}
  function retry(){void run(applied??{limit:HISTORY_DEFAULT_LIMIT,offset:0});}
  onMount(()=>{void run({limit:HISTORY_DEFAULT_LIMIT,offset:0});return()=>{alive=false;++epoch;};});
</script>
<section class="history" aria-label="History" aria-busy={loading}>
  <h2>History</h2>
  <p>Read-only list of imported-file events: native imports and events from uploaded Sonarr/Radarr snapshots. Nothing here can be changed.</p>
  <ul>{#each HISTORY_NOTES as note (note)}<li>{note}</li>{/each}</ul>
  <form onsubmit={apply} aria-label="History filters">
    <label>Media type
      <select bind:value={form.mediaType}><option value="">Any</option><option value="tv">TV</option><option value="movies">Movies</option></select>
    </label>
    <label>Series ID<input bind:value={form.seriesId} inputmode="numeric" autocomplete="off" /></label>
    <label>Season (requires series)<input bind:value={form.season} inputmode="numeric" autocomplete="off" /></label>
    <label>Episode ID<input bind:value={form.episodeId} inputmode="numeric" autocomplete="off" /></label>
    <label>Movie ID<input bind:value={form.movieId} inputmode="numeric" autocomplete="off" /></label>
    <label>From (inclusive, RFC3339 with timezone)<input bind:value={form.from} placeholder="2026-01-02T03:04:05Z" autocomplete="off" /></label>
    <label>To (exclusive, RFC3339 with timezone)<input bind:value={form.to} placeholder="2026-01-03T03:04:05+02:00" autocomplete="off" /></label>
    <label>Limit (1–{HISTORY_MAX_LIMIT})<input bind:value={form.limit} inputmode="numeric" placeholder={String(HISTORY_DEFAULT_LIMIT)} autocomplete="off" /></label>
    <label>Offset (0–{HISTORY_MAX_OFFSET})<input bind:value={form.offset} inputmode="numeric" placeholder="0" autocomplete="off" /></label>
    <p>Movie ID cannot be combined with TV selectors or media type TV; TV selectors cannot be combined with media type Movies.</p>
    {#if formErrors.length}<div role="alert"><p>Fix these filters before applying:</p><ul>{#each formErrors as message (message)}<li>{message}</li>{/each}</ul></div>{/if}
    <button type="submit" disabled={loading}>Apply filters</button>
    <button type="button" disabled={loading} onclick={reset}>Reset</button>
  </form>
  {#if loading}<p role="status">Loading history…</p>{/if}
  {#if failure}
    <div role="alert">
      <h3>{failure.title}</h3>
      <p>{failure.advice}</p>
      <p>Error code: <code>{failure.code}</code>{#if failure.status!==null} (HTTP {failure.status}){/if}. {failure.message}</p>
      <button type="button" disabled={loading} onclick={retry}>Retry</button>
    </div>
  {/if}
  {#if page}
    <p role="status">{page.rangeLabel}{#if failure} (previous results, may be out of date){/if}</p>
    {#if page.events.length===0}
      <p>No history events match these filters.</p>
    {:else}
      <ol class="history-list" aria-label="History events">
        {#each page.events as event (event.key)}
          <li>
            <h3>{event.eventLabel} <small>({event.domain}: {event.target})</small></h3>
            <p><strong>{event.originLabel}</strong></p>
            <p>{event.timeLabel}: <time datetime={event.time}>{event.time}</time></p>
            {#if event.sourceEventType!==null}<p>Original source event type: <code>{event.sourceEventType}</code>{#if event.sourceCodeNote} — {event.sourceCodeNote}{/if}</p>{/if}
            {#each event.warnings as warning (warning)}<p role="alert">{warning}</p>{/each}
            <dl>{#each event.facts as item (item.label)}<dt>{item.label}</dt><dd>{#if item.recorded}<code>{item.value}</code>{:else}<em>{item.value}</em>{/if}</dd>{/each}</dl>
          </li>
        {/each}
      </ol>
    {/if}
    <nav aria-label="History pages">
      <button type="button" disabled={loading||page.prevOffset===null} onclick={()=>go(page?.prevOffset??null)}>Previous page</button>
      <button type="button" disabled={loading||page.nextOffset===null} onclick={()=>go(page?.nextOffset??null)}>Next page</button>
    </nav>
    {#if page.beyondOffsetCap}<p>More events exist beyond offset {HISTORY_MAX_OFFSET}, which the API cannot page to. Narrow the media type, selectors or dates.</p>{/if}
  {:else if !loading && !failure}
    <p role="status">History has not been loaded.</p>
  {/if}
</section>
