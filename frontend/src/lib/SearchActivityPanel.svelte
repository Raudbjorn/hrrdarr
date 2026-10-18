<script lang="ts">
  // Activity: every durable search command (Wanted bulk/search-all, library detail searches),
  // newest first, polled while Activity is visible. Read-only; cancellation and results stay
  // in the per-target search panel.
  import { onMount, untrack } from 'svelte';
  import type { ApiPage, CommandStatus, SearchCommand } from './api.generated';
  import { listAllSearchCommands } from './api';
  import { SEARCH_ACTIVITY_IDLE_POLL_MS, SEARCH_ACTIVITY_PAGE_SIZE, SEARCH_STATUSES, STATUS_TEXT, nextPollMs, outcomeLabel, searchActivityQuery, statusSummary, targetLabel } from './search-activity';
  let { active = true }: { active?: boolean } = $props();

  let status = $state<'' | CommandStatus>(''), offset = $state(0);
  let page = $state<ApiPage<SearchCommand> | null>(null), loading = $state(false), error = $state('');
  let alive = true, version = 0, timer: ReturnType<typeof setTimeout> | undefined;
  const date = (seconds: number | null) => seconds === null ? 'Not recorded' : new Date(seconds * 1000).toLocaleString();
  const summary = $derived(page ? statusSummary(page.total, page.items) : '');

  function schedule(ms: number) {
    clearTimeout(timer);
    if (alive && active && !document.hidden) timer = setTimeout(() => void read(), ms);
  }
  async function read() {
    if (!alive || !active || document.hidden) return;
    const token = ++version; clearTimeout(timer); loading = true;
    const result = await listAllSearchCommands(searchActivityQuery(status, offset));
    if (!alive || token !== version) return;
    loading = false;
    if (result.ok) { page = result.data; error = ''; } else error = result.error;
    schedule(result.ok ? nextPollMs(result.data.items) : SEARCH_ACTIVITY_IDLE_POLL_MS);
  }
  function filter(next: '' | CommandStatus) { status = next; offset = 0; void read(); }
  function move(next: number) { offset = Math.max(0, next); void read(); }

  $effect(() => {
    const shown = active;
    untrack(() => { if (shown) void read(); else { ++version; loading = false; clearTimeout(timer); } });
  });
  onMount(() => {
    alive = true;
    const visibility = () => { clearTimeout(timer); if (!document.hidden && active) void read(); };
    document.addEventListener('visibilitychange', visibility);
    return () => { alive = false; ++version; clearTimeout(timer); document.removeEventListener('visibilitychange', visibility); };
  });
</script>

<section class="surface" aria-labelledby="search-activity-heading" aria-busy={loading}>
  <h2 id="search-activity-heading">Search commands</h2>
  <p>Release searches admitted from Wanted and library detail, newest first. This list re-reads every {SEARCH_ACTIVITY_IDLE_POLL_MS / 1000} seconds while Activity is visible, and sooner while a search is queued or running.</p>
  <div class="controls">
    <label>Search status<select value={status} onchange={(event) => filter(event.currentTarget.value as '' | CommandStatus)}><option value="">All statuses</option>{#each SEARCH_STATUSES as value (value)}<option value={value}>{STATUS_TEXT[value]}</option>{/each}</select></label>
    <button type="button" disabled={loading} onclick={() => read()}>Refresh search commands</button>
  </div>
  {#if loading && !page}<p role="status">Loading search commands…</p>{/if}
  {#if error}<p class="error" role="alert">Search commands could not be loaded: {error}{page ? ' Showing the last successful read.' : ''}</p><button type="button" disabled={loading} onclick={() => read()}>Retry loading search commands</button>{/if}
  <p aria-live="polite">{summary}</p>
  {#if page}
    <div class="scroll"><table aria-label="Search command history"><thead><tr><th scope="col">Target</th><th scope="col">Status</th><th scope="col">Attempts / outcome</th><th scope="col">Releases fetched</th><th scope="col">Created</th><th scope="col">Command</th></tr></thead>
      <tbody>{#each page.items as command (command.id)}<tr data-status={command.status}><th scope="row">{targetLabel(command.target)}<small>{command.source.media_type === 'tv' ? 'TV' : 'Movies'} · {command.mode} search</small></th><td>{STATUS_TEXT[command.status]}</td><td>{command.attempts} / {outcomeLabel(command)}</td><td>{command.fetched}{command.fetch_complete ? '' : ' (incomplete)'}</td><td>{date(command.created_at)}<small>Started {date(command.started_at)}; completed {date(command.completed_at)}</small></td><td><small>{command.id}</small></td></tr>
      {:else}<tr><td colspan="6">{status ? `No ${STATUS_TEXT[status].toLowerCase()} search commands.` : 'No search commands yet. Searches started from Wanted or library detail appear here.'}</td></tr>{/each}</tbody></table></div>
    {#if page.total > SEARCH_ACTIVITY_PAGE_SIZE}<div class="actions"><button type="button" disabled={loading || offset === 0} onclick={() => move(offset - SEARCH_ACTIVITY_PAGE_SIZE)}>Previous search commands</button><button type="button" disabled={loading || offset + SEARCH_ACTIVITY_PAGE_SIZE >= page.total} onclick={() => move(offset + SEARCH_ACTIVITY_PAGE_SIZE)}>Next search commands</button></div>{/if}
  {/if}
</section>

<style>
  .surface{background:white;border:1px solid #cbd5de;padding:1.25rem;margin:1rem 0}.surface h2{margin-top:0}.controls{display:flex;flex-wrap:wrap;gap:1rem;margin-bottom:1rem;align-items:end}.controls label{min-width:180px}.scroll{overflow-x:auto}table{width:100%;border-collapse:collapse;text-align:left}th,td{padding:.7rem;border-bottom:1px solid #dce3e9;vertical-align:top}small{display:block;overflow-wrap:anywhere;color:#596b7b;font-weight:400}.actions{margin-top:1rem}@media(max-width:700px){.surface{padding:1rem}.controls{display:block}}
</style>
