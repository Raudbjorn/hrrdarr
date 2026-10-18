<script lang="ts">
  import { onDestroy, tick } from 'svelte';
  import type { MovieRenamePreview, MovieRenamePreviewReason } from './api.generated';
  import type { FolderRenamePreview, RenameCommand, RenameCommandDetail } from './api.generated';
  import { getFolderRenamePreview, getMovieRenamePreview } from './api';
  import { cancelRename, isTerminal, reasonText, runRename, selectedChangeIds, summarize } from './rename-state';
  let { movieId, onrenamed }: { movieId: number; onrenamed?: () => void } = $props();
  let folder: FolderRenamePreview | null = $state(null);
  let selected: Set<number> = $state(new Set());
  let running = $state(false), command: RenameCommand | RenameCommandDetail | null = $state(null), runError = $state('');
  let loading = $state(false), opened = $state(false), error = $state('');
  let result: MovieRenamePreview | null = $state(null);
  let alive = true, generation = 0;
  let previewButton: HTMLButtonElement;
  const reasons: Record<MovieRenamePreviewReason, string> = {
    invalid_path: 'The stored file path is invalid or outside the movie folder.',
    missing_extension: 'The stored file has no usable extension.',
    invalid_file_facts: 'Stored file facts required by this naming pattern are invalid.',
    naming_render_failed: 'This file cannot produce a safe name from the naming pattern.',
  };
  async function load() {
    const captured = movieId, version = ++generation;
    opened = true; loading = true; error = ''; result = null; folder = null; selected = new Set();
    const [response, folders] = await Promise.all([getMovieRenamePreview([captured]), getFolderRenamePreview('movies', [captured])]);
    if (!alive || version !== generation || captured !== movieId) return;
    loading = false;
    if (!response.ok) { error = response.error; return; }
    result = response.data;
    folder = folders.ok ? folders.data : null;
    selected = new Set(response.data.items.filter(i => i.status === 'change').map(i => i.movie_file_id));
  }
  const rows = $derived.by(() => ((result as MovieRenamePreview | null)?.items ?? []).map(i => ({ id: i.movie_file_id, status: i.status })));
  const picked = $derived(selectedChangeIds(rows, selected));
  function toggle(id: number) {
    const next = new Set(selected);
    if (next.has(id)) next.delete(id); else next.add(id);
    selected = next;
  }
  async function execute(name: 'rename_files' | 'rename_movie' | 'rename_movie_folder', fileIds: number[]) {
    if (!result || running) return;
    const captured = movieId, revision = result.naming_revision;
    running = true; runError = ''; command = null;
    const outcome = await runRename({
      name,
      target: { media_type: 'movies', series_ids: [], movie_ids: [captured], file_ids: name === 'rename_files' ? fileIds : [] },
      expected_naming_revision: revision,
      priority: 'normal',
    }, c => { if (alive && captured === movieId) command = c; }, () => alive && captured === movieId);
    if (!alive || captured !== movieId) return;
    running = false;
    if (!outcome.ok) { runError = outcome.commandId ? `${outcome.error} (command ${outcome.commandId})` : outcome.error; return; }
    onrenamed?.();
    await load();
  }
  async function cancel() {
    if (command && !isTerminal(command)) {
      const r = await cancelRename(command.id);
      if (!r.ok && alive) runError = r.error;
    }
  }
  async function close() {
    const version = ++generation;
    opened = false; loading = false; error = ''; result = null; folder = null; selected = new Set();
    await tick();
    if (alive && generation === version) previewButton?.focus();
  }
  onDestroy(() => { alive = false; ++generation; });
</script>

<section aria-label="Movie filename preview" aria-busy={loading}>
  <h2>Filename preview</h2>
  <button bind:this={previewButton} type="button" disabled={loading || running} onclick={() => void load()}>{opened ? 'Reload filename preview' : 'Preview filenames'}</button>
  {#if opened}<button type="button" onclick={close}>Close filename preview</button>{/if}
  <p class="muted">The preview does not check destination occupancy or links. Renaming moves the file with its subtitle and NFO files on disk and re-checks every destination before it moves anything.</p>
  {#if loading}
    <p role="status">Loading filename preview…</p>
  {:else if error}
    <p class="error" role="alert">{error}</p>
  {:else if result}
    <p>Naming revision {result.naming_revision}. Paths are relative to this movie’s folder.</p>
    {#if result.rename_enabled}
      <p class="pattern">Pattern: <code>{result.standard_movie_format}</code></p>
    {:else}
      <p>Renaming is disabled. The preview uses the original release title when available, otherwise the current filename.</p>
    {/if}
    <p>{result.files_considered} files considered; {result.unchanged_count} unchanged; {result.unavailable_count} unavailable.</p>
    {#if result.files_considered === 0}<p>No movie files to preview.</p>
    {:else if result.items.length === 0}<p>No filename changes.</p>{/if}
    {#if result.items.length}
      <ul>
        {#each result.items as item (item.movie_file_id)}
          <li>
            {#if item.status === 'change'}
              <label><input type="checkbox" checked={selected.has(item.movie_file_id)} disabled={running} onchange={() => toggle(item.movie_file_id)} /> File {item.movie_file_id}</label>
            {:else}<p>File {item.movie_file_id}</p>{/if}
            <dl><dt>Existing path</dt><dd>{item.existing_path ?? 'Unavailable'}</dd><dt>Proposed path</dt><dd>{item.new_path ?? 'Unavailable'}</dd></dl>
            {#each item.reasons as reason}<p>{reasons[reason]}</p>{/each}
          </li>
        {/each}
      </ul>
      <div class="actions">
        <button type="button" disabled={running || picked.length === 0} onclick={() => void execute('rename_files', picked)}>Rename selected ({picked.length})</button>
        <button type="button" disabled={running || rows.every((r: { status: string }) => r.status !== 'change')} onclick={() => void execute('rename_movie', [])}>Rename all</button>
      </div>
    {/if}
    {#if folder}
      <h3>Movie folder</h3>
      {#each folder.items as f (f.movie_id)}
        <dl><dt>Existing folder</dt><dd>{f.existing_path}</dd><dt>Proposed folder</dt><dd>{f.new_path ?? 'Unavailable'}</dd></dl>
        {#each f.reasons as reason}<p>{reasonText[reason]}</p>{/each}
        {#if f.status === 'change'}<button type="button" disabled={running} onclick={() => void execute('rename_movie_folder', [])}>Rename movie folder</button>{/if}
      {:else}
        <p>The movie folder already matches its naming format.</p>
      {/each}
    {/if}
  {/if}
  {#if command}
    <p role="status">{summarize(command as RenameCommandDetail)}</p>
    {#if running && !isTerminal(command)}<button type="button" onclick={() => void cancel()}>Cancel rename</button>{/if}
    {#if 'items' in command}{#each command.items.filter(i => i.state !== 'committed') as item}<p>Not renamed: {item.old_path}{item.error_code ? ` (${item.error_code})` : ''}</p>{/each}{/if}
  {/if}
  {#if runError}<p class="error" role="alert">{runError}</p>{/if}
</section>

<style>
  section { margin-top: 1.5rem; }
  li, .pattern, dd, p { overflow-wrap: anywhere; }
  dl, dd { min-width: 0; }
  input[type="checkbox"] { width: auto; margin: 0 .4rem 0 0; }
  ul { padding-inline-start: 1.3rem; }
  .actions { display: flex; gap: .5rem; flex-wrap: wrap; }
  li { margin-block: 1rem; }
</style>
