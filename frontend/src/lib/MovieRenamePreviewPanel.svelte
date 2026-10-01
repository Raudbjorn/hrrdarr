<script lang="ts">
  import { onDestroy, tick } from 'svelte';
  import type { MovieRenamePreview, MovieRenamePreviewReason } from './api.generated';
  import { getMovieRenamePreview } from './api';
  let { movieId }: { movieId: number } = $props();
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
    opened = true; loading = true; error = ''; result = null;
    const response = await getMovieRenamePreview([captured]);
    if (!alive || version !== generation || captured !== movieId) return;
    loading = false;
    if (!response.ok) { error = response.error; return; }
    result = response.data;
  }
  async function close() {
    const version = ++generation;
    opened = false; loading = false; error = ''; result = null;
    await tick();
    if (alive && generation === version) previewButton?.focus();
  }
  onDestroy(() => { alive = false; ++generation; });
</script>

<section aria-label="Movie filename preview" aria-busy={loading}>
  <h2>Filename preview</h2>
  <button bind:this={previewButton} type="button" disabled={loading} onclick={() => void load()}>{opened ? 'Reload filename preview' : 'Preview filenames'}</button>
  {#if opened}<button type="button" onclick={close}>Close filename preview</button>{/if}
  <p class="muted">Preview only. Destination occupancy, symlinks, and execution safety are not checked.</p>
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
            <p>File {item.movie_file_id}</p>
            <dl><dt>Existing path</dt><dd>{item.existing_path ?? 'Unavailable'}</dd><dt>Proposed path</dt><dd>{item.new_path ?? 'Unavailable'}</dd></dl>
            {#each item.reasons as reason}<p>{reasons[reason]}</p>{/each}
          </li>
        {/each}
      </ul>
    {/if}
  {/if}
</section>

<style>
  section { margin-top: 1.5rem; }
  li, .pattern { overflow-wrap: anywhere; }
  ul { padding-inline-start: 1.3rem; }
  li { margin-block: 1rem; }
</style>
