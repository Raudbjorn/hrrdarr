<script lang="ts">
  // Calendar: read-only month/week/agenda views over the native TV and movie calendar APIs.
  // Nothing here writes or searches. Responses for a superseded range or filter are ignored,
  // and polling never overlaps a running read.
  import { onMount, tick } from 'svelte';
  import type { CalendarPage, MediaDomain, MovieCalendarEntry, MovieReleaseType, TvCalendarEntry } from './api.generated';
  import { listCalendar } from './api';
  import {
    POLL_INTERVAL_MS, RELEASE_LABELS, RELEASE_TYPES, STATUS_LABELS, WEEKDAYS, countLabel, dateLabel, defaultOptions, feedPath,
    groupByDate, inRange, localDateOf, moveDate, movieItem, movieQuery, rangeFor, rangeTitle, step, tvItem, tvQuery, webcalUrl, weeksOf,
    type CalendarItem, type CalendarOptions, type CalendarScope, type CalendarStatus, type CalendarView, type LocalDate,
  } from './calendar-view';

  const MONTH_CELL_ENTRIES = 4;
  const DOMAIN_NAMES: Record<MediaDomain, string> = { tv: 'TV', movies: 'Movies' };

  let scope = $state<CalendarScope>('both');
  let mode = $state<CalendarView>('month');
  let today = $state<LocalDate>(localDateOf(new Date()));
  let anchor = $state<LocalDate>(localDateOf(new Date()));
  let focusDate = $state<LocalDate>(localDateOf(new Date()));
  let selectedDate = $state<LocalDate | null>(null);
  let options = $state<CalendarOptions>(defaultOptions());
  let tvPage = $state<CalendarPage<TvCalendarEntry> | null>(null), moviePage = $state<CalendarPage<MovieCalendarEntry> | null>(null);
  let tvError = $state(''), movieError = $state(''), loading = $state(false), loadedOnce = $state(false);
  let clockMs = $state(Date.now());
  let copyNotice = $state(''), copyError = $state('');
  let grid = $state<HTMLElement | null>(null);
  let readVersion = 0, alive = true;

  const range = $derived(rangeFor(mode, anchor));
  const title = $derived(rangeTitle(mode, anchor));
  const unit = $derived(mode === 'week' ? 'week' : 'month');
  const showsTv = $derived(scope !== 'movies'), showsMovies = $derived(scope !== 'tv');
  const noReleaseTypes = $derived(showsMovies && options.releaseTypes.length === 0);
  const items = $derived<CalendarItem[]>([
    ...(showsTv && tvPage ? tvPage.items.map((entry) => tvItem(entry, clockMs)) : []),
    ...(showsMovies && moviePage ? moviePage.items.map((entry) => movieItem(entry, today)) : []),
  ]);
  const groups = $derived(groupByDate(items, range));
  const weeks = $derived(mode === 'agenda' ? [] : weeksOf(range));
  const visibleCount = $derived([...groups.values()].reduce((total, day) => total + day.length, 0));
  const errors = $derived([showsTv && tvError ? `TV calendar could not be loaded: ${tvError}` : '', showsMovies && movieError ? `Movie calendar could not be loaded: ${movieError}` : ''].filter(Boolean));
  const notes = $derived.by(() => {
    const out: string[] = [];
    const tvD = showsTv ? tvPage?.diagnostics : undefined, movieD = showsMovies ? moviePage?.diagnostics : undefined;
    if (tvD?.date_only_air_dates) out.push(`${tvD.date_only_air_dates} ${tvD.date_only_air_dates === 1 ? 'episode has' : 'episodes have'} an air date in this range but no known air time, so ${tvD.date_only_air_dates === 1 ? 'it is' : 'they are'} not shown.`);
    const invalid = (tvD?.invalid_dates ?? 0) + (movieD?.invalid_dates ?? 0);
    if (invalid) out.push(`${invalid} stored ${invalid === 1 ? 'date is' : 'dates are'} invalid and not shown; the calendar feed refuses to publish until ${invalid === 1 ? 'it is' : 'they are'} repaired.`);
    return out;
  });
  const feeds = $derived((['tv', 'movies'] as MediaDomain[]).filter((d) => d === 'tv' ? showsTv : showsMovies).map((domain) => {
    const path = feedPath(domain, options);
    const url = path && typeof location !== 'undefined' ? `${location.origin}${path}` : null;
    return { domain, url, webcal: url ? webcalUrl(url) : null };
  }));

  async function load() {
    const version = ++readVersion, r = range, opts = $state.snapshot(options), wantTv = showsTv, wantMovies = showsMovies;
    const mq = wantMovies ? movieQuery(r, opts) : null;
    loading = true;
    const [tv, movies] = await Promise.all([
      wantTv ? listCalendar<TvCalendarEntry>('tv', tvQuery(r, opts)) : Promise.resolve(null),
      mq ? listCalendar<MovieCalendarEntry>('movies', mq) : Promise.resolve(null),
    ]);
    if (!alive || version !== readVersion) return;
    loading = false; loadedOnce = true; clockMs = Date.now(); today = localDateOf(new Date(clockMs));
    if (tv) {
      if (!tv.ok) { tvError = tv.error; tvPage = null; }
      else if (tv.data.media_type !== 'tv') { tvError = 'Unexpected calendar response.'; tvPage = null; }
      else { tvError = ''; tvPage = tv.data; }
    }
    if (wantMovies && !mq) { movieError = ''; moviePage = null; }
    if (movies) {
      if (!movies.ok) { movieError = movies.error; moviePage = null; }
      else if (movies.data.media_type !== 'movies') { movieError = 'Unexpected calendar response.'; moviePage = null; }
      else { movieError = ''; moviePage = movies.data; }
    }
  }
  function focusDay(date: LocalDate) {
    void tick().then(() => grid?.querySelector<HTMLButtonElement>(`button[data-date="${date}"]`)?.focus());
  }
  function go(direction: -1 | 1) {
    anchor = step(mode, anchor, direction); focusDate = anchor; selectedDate = null; void load();
  }
  function goToday() {
    today = localDateOf(new Date()); anchor = today; focusDate = today; selectedDate = null; void load();
  }
  function changeView(next: CalendarView) {
    if (next === mode) return;
    mode = next; void load();
  }
  function changeScope(next: CalendarScope) {
    if (next === scope) return;
    scope = next; tvError = ''; movieError = ''; void load();
  }
  function setOption(patch: Partial<CalendarOptions>) {
    options = { ...options, ...patch }; void load();
  }
  function toggleRelease(type: MovieReleaseType, on: boolean) {
    setOption({ releaseTypes: RELEASE_TYPES.filter((t) => t === type ? on : options.releaseTypes.includes(t)) });
  }
  function selectDay(date: LocalDate) {
    selectedDate = date; focusDate = date;
  }
  function dayKey(event: KeyboardEvent, date: LocalDate) {
    if (event.key === 'Enter' || event.key === ' ') { event.preventDefault(); selectDay(date); return; }
    const next = moveDate(date, event.key);
    if (!next) return;
    event.preventDefault();
    focusDate = next;
    if (!inRange(next, range)) { anchor = next; void load(); }
    focusDay(next);
  }
  async function copy(url: string, domain: MediaDomain) {
    copyNotice = ''; copyError = '';
    try { await navigator.clipboard.writeText(url); copyNotice = `${DOMAIN_NAMES[domain]} feed link copied.`; }
    catch { copyError = 'Copying is not available here; select the link text and copy it manually.'; }
  }
  const statusMark: Record<CalendarStatus, string> = { downloaded: '✓', missing: '!', airing: '●', unaired: '○', upcoming: '○', unmonitored: '–' };

  onMount(() => {
    alive = true; void load();
    const timer = setInterval(() => { if (!loading && document.visibilityState === 'visible') void load(); }, POLL_INTERVAL_MS);
    return () => { alive = false; ++readVersion; clearInterval(timer); };
  });
</script>

{#snippet entry(item: CalendarItem, compact: boolean)}
  <li class={`entry ${item.domain} ${item.releaseType ? `type-${item.releaseType}` : ''} status-${item.status}`}>
    <span class="when">{item.time ?? 'All day'}</span>
    <span class="name">{item.title}</span>
    <span class="detail">{item.detail}</span>
    <span class="status"><span aria-hidden="true">{statusMark[item.status]}</span> <span class:sr-only={compact}>{item.statusLabel}</span></span>
  </li>
{/snippet}

<section class="calendar" aria-labelledby="calendar-heading">
  <h1 id="calendar-heading">Calendar</h1>
  <div class="toolbar">
    <div role="group" aria-label="Calendar navigation">
      <button disabled={loading} onclick={() => go(-1)}>Previous {unit}</button>
      <button disabled={loading} onclick={goToday}>Today</button>
      <button disabled={loading} onclick={() => go(1)}>Next {unit}</button>
    </div>
    <h2 id="calendar-range" aria-live="polite">{title}</h2>
    <nav aria-label="Calendar view"><button aria-pressed={mode === 'month'} onclick={() => changeView('month')}>Month</button><button aria-pressed={mode === 'week'} onclick={() => changeView('week')}>Week</button><button aria-pressed={mode === 'agenda'} onclick={() => changeView('agenda')}>Agenda</button></nav>
    <nav aria-label="Calendar media"><button aria-pressed={scope === 'tv'} onclick={() => changeScope('tv')}>TV</button><button aria-pressed={scope === 'movies'} onclick={() => changeScope('movies')}>Movies</button><button aria-pressed={scope === 'both'} onclick={() => changeScope('both')}>Both</button></nav>
  </div>

  <form class="filters" aria-label="Calendar filters" onsubmit={(event) => event.preventDefault()}>
    <label class="check"><input type="checkbox" checked={options.unmonitored} onchange={(e) => setOption({ unmonitored: e.currentTarget.checked })} /> Include unmonitored</label>
    {#if showsTv}<label class="check"><input type="checkbox" checked={options.specials} onchange={(e) => setOption({ specials: e.currentTarget.checked })} /> Include specials</label>{/if}
    {#if showsMovies}
      <fieldset><legend>Movie release types</legend>
        {#each RELEASE_TYPES as type (type)}<label class="check"><input type="checkbox" checked={options.releaseTypes.includes(type)} onchange={(e) => toggleRelease(type, e.currentTarget.checked)} /> <span class={`swatch type-${type}`} aria-hidden="true"></span>{RELEASE_LABELS[type]}</label>{/each}
      </fieldset>
    {/if}
    <button type="button" disabled={loading} onclick={() => load()}>Refresh calendar</button>
  </form>

  <section class="legend" aria-labelledby="calendar-legend-heading">
    <h2 id="calendar-legend-heading">Legend</h2>
    <ul>
      <li><span class="swatch tv" aria-hidden="true"></span>TV episode (local air time)</li>
      {#each RELEASE_TYPES as type (type)}<li><span class={`swatch type-${type}`} aria-hidden="true"></span>Movie: {RELEASE_LABELS[type]} (all day)</li>{/each}
      {#each Object.entries(STATUS_LABELS) as [status, label] (status)}<li><span class="mark" aria-hidden="true">{statusMark[status as CalendarStatus]}</span>{label}</li>{/each}
    </ul>
  </section>

  {#if loading}<p role="status">Loading calendar…</p>{/if}
  {#each errors as message (message)}<p role="alert" class="error">{message}</p>{/each}
  {#if errors.length}<button disabled={loading} onclick={() => load()}>Retry loading calendar</button>{/if}
  {#if noReleaseTypes}<p role="note">Choose at least one movie release type to show movies.</p>{/if}
  {#each notes as note (note)}<p role="note">{note}</p>{/each}
  {#if loadedOnce && !loading && !errors.length && visibleCount === 0}<p class="empty" role="status">Nothing scheduled in {title} for these filters.</p>{/if}

  {#if mode === 'agenda'}
    <section class="agenda" aria-label="Agenda" aria-busy={loading}>
      {#each [...groups.entries()] as [date, dayItems] (date)}
        <section aria-labelledby={`agenda-${date}`}>
          <h3 id={`agenda-${date}`}>{dateLabel(date)}{date === today ? ' (today)' : ''}</h3>
          <ul aria-label={`Entries for ${dateLabel(date)}`}>{#each dayItems as item (item.key)}{@render entry(item, false)}{/each}</ul>
        </section>
      {/each}
    </section>
  {:else}
    <div class="grid-scroll">
      <table role="grid" aria-labelledby="calendar-range" aria-busy={loading} bind:this={grid}>
        <thead><tr>{#each WEEKDAYS as name (name)}<th scope="col" abbr={name}>{name.slice(0, 3)}</th>{/each}</tr></thead>
        <tbody>
          {#each weeks as week (week[0])}
            <tr>
              {#each week as date (date)}
                {@const dayItems = groups.get(date) ?? []}
                <td role="gridcell" aria-selected={selectedDate === date} class:outside={mode === 'month' && date.slice(0, 7) !== anchor.slice(0, 7)} class:today={date === today}>
                  <button class="day" type="button" data-date={date} tabindex={date === focusDate ? 0 : -1} aria-current={date === today ? 'date' : undefined}
                    aria-label={`${dateLabel(date)}${date === today ? ' (today)' : ''}: ${countLabel(dayItems.length)}`}
                    onclick={() => selectDay(date)} onkeydown={(event) => dayKey(event, date)} onfocus={() => { focusDate = date; }}>{Number(date.slice(8))}</button>
                  {#if dayItems.length}
                    <ul>{#each (mode === 'month' ? dayItems.slice(0, MONTH_CELL_ENTRIES) : dayItems) as item (item.key)}{@render entry(item, mode === 'month')}{/each}</ul>
                    {#if mode === 'month' && dayItems.length > MONTH_CELL_ENTRIES}<p class="more">+{dayItems.length - MONTH_CELL_ENTRIES} more</p>{/if}
                  {/if}
                </td>
              {/each}
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
    <p class="muted">Use the arrow keys to move between days, Home/End for the week, Page Up/Down for months, and Enter to show a day's details.</p>
  {/if}

  {#if selectedDate && mode !== 'agenda'}
    {@const dayItems = groups.get(selectedDate) ?? []}
    <section class="details" aria-labelledby="calendar-day-heading">
      <h2 id="calendar-day-heading">{dateLabel(selectedDate)}</h2>
      {#if dayItems.length}<ul aria-label={`Entries for ${dateLabel(selectedDate)}`}>{#each dayItems as item (item.key)}{@render entry(item, false)}{/each}</ul>
      {:else}<p>Nothing scheduled on this day.</p>{/if}
      <button type="button" onclick={() => { const date = selectedDate; selectedDate = null; if (date) focusDay(date); }}>Close day details</button>
    </section>
  {/if}

  <section class="feeds" aria-labelledby="calendar-feeds-heading">
    <h2 id="calendar-feeds-heading">iCalendar feeds</h2>
    <p class="muted">Subscriptions use the current filters, cover 7 days back to 28 days ahead (UTC dates), and are reachable only where this page is. No credentials are added to the link.</p>
    {#each feeds as feed (feed.domain)}
      <div class="feed">
        {#if feed.url}
          <label>{DOMAIN_NAMES[feed.domain]} feed link<input readonly value={feed.url} onfocus={(e) => e.currentTarget.select()} /></label>
          <button type="button" onclick={() => copy(feed.url!, feed.domain)}>Copy {DOMAIN_NAMES[feed.domain]} feed link</button>
          {#if feed.webcal}<a href={feed.webcal}>Subscribe to {DOMAIN_NAMES[feed.domain]} feed</a>{/if}
        {:else}<p>{DOMAIN_NAMES[feed.domain]} feed needs at least one release type.</p>{/if}
      </div>
    {/each}
    {#if copyNotice}<p role="status">{copyNotice}</p>{/if}{#if copyError}<p role="alert" class="error">{copyError}</p>{/if}
  </section>
</section>

<style>
  .calendar{background:#fff;border:1px solid #cbd5de;padding:1.5rem}
  .toolbar{display:flex;flex-wrap:wrap;gap:1rem;align-items:center;justify-content:space-between}
  .toolbar h2{margin:0;font-size:1.25rem;min-width:12rem}
  .filters{display:flex;flex-wrap:wrap;gap:.8rem;align-items:center;max-width:none;margin:1rem 0}
  .filters fieldset{display:flex;flex-wrap:wrap;gap:.6rem;border:1px solid #dce3e9;padding:.3rem .7rem}
  .check{display:inline-flex;align-items:center;gap:.35rem;font-weight:600}
  .check input{width:auto;margin:0}
  .legend h2{font-size:1rem;margin:.5rem 0}
  .legend ul{display:flex;flex-wrap:wrap;gap:.4rem 1rem;list-style:none;padding:0;margin:0 0 1rem}
  .legend li{display:inline-flex;align-items:center;gap:.35rem;font-size:.85rem}
  .swatch{display:inline-block;width:.9rem;height:.9rem;border-left:4px solid #245ba8;background:#e8f0fb}
  .swatch.type-in_cinemas{border-color:#8a3ab9;background:#f4eafa}
  .swatch.type-digital{border-color:#1f7a6a;background:#e6f5f2}
  .swatch.type-physical{border-color:#a35a00;background:#fbefe0}
  .mark{display:inline-block;min-width:1rem;text-align:center;font-weight:700}
  .grid-scroll{overflow-x:auto}
  table{border-collapse:collapse;width:100%;table-layout:fixed;min-width:640px}
  th{font-size:.85rem;padding:.3rem;border-bottom:1px solid #cbd5de}
  td{vertical-align:top;border:1px solid #dce3e9;padding:.25rem;height:6.5rem}
  td.outside{background:#f6f8fa;color:#5b6773}
  td.today{box-shadow:inset 0 0 0 2px #245ba8}
  td[aria-selected="true"]{background:#eef4fc}
  .day{min-width:2rem;padding:.1rem .4rem;font-weight:600}
  td ul,.agenda ul,.details ul{list-style:none;padding:0;margin:.25rem 0 0}
  .entry{display:grid;grid-template-columns:auto 1fr;gap:0 .35rem;font-size:.8rem;border-left:4px solid #245ba8;background:#e8f0fb;padding:.15rem .3rem;margin-bottom:.2rem;overflow-wrap:anywhere}
  .entry.type-in_cinemas{border-color:#8a3ab9;background:#f4eafa}
  .entry.type-digital{border-color:#1f7a6a;background:#e6f5f2}
  .entry.type-physical{border-color:#a35a00;background:#fbefe0}
  .entry.status-unmonitored{opacity:.75;border-left-style:dashed}
  .entry .name{font-weight:600}
  .entry .detail,.entry .status{grid-column:1 / -1}
  td .entry{display:flex;flex-direction:column}
  .agenda .entry,.details .entry{font-size:.95rem;grid-template-columns:5rem 1fr}
  .agenda h3{font-size:1rem;margin:1rem 0 .25rem}
  .more{font-size:.75rem;margin:.1rem 0}
  .details,.feeds{border-top:2px solid #245ba8;margin-top:1.5rem;padding-top:1rem}
  .feed{display:flex;flex-wrap:wrap;gap:.6rem;align-items:end;margin:.6rem 0}
  .feed input{min-width:min(36rem,80vw)}
  .empty{font-style:italic}
  .sr-only{position:absolute;width:1px;height:1px;padding:0;margin:-1px;overflow:hidden;clip:rect(0,0,0,0);white-space:nowrap;border:0}
  @media(max-width:800px){.calendar{padding:1rem}td{height:auto}}
</style>
