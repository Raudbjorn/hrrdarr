<script lang="ts">
  import { onDestroy, tick } from 'svelte';
  import type { FolderRenamePreview, RenameCommand, RenameCommandDetail, TvRenamePreview } from './api.generated';
  import { getFolderRenamePreview, getTvRenamePreview } from './api';
  import { cancelRename, changeIds, isTerminal, reasonText, runRename, summarize, selectedChangeIds } from './rename-state';
  let { seriesId, onrenamed }: { seriesId: number; onrenamed?: () => void } = $props();
  let loading = $state(false), opened = $state(false), error = $state('');
  let result: TvRenamePreview | null = $state(null);
  let folder: FolderRenamePreview | null = $state(null);
  let selected: Set<number> = $state(new Set());
  let running = $state(false), command: RenameCommand | RenameCommandDetail | null = $state(null), runError = $state('');
  let alive = true, generation = 0;
  let previewButton: HTMLButtonElement;
  const rows = $derived.by(() => ((result as TvRenamePreview | null)?.items ?? []).map(i => ({ id: i.episode_file_id, status: i.status })));
  const picked = $derived(selectedChangeIds(rows, selected));
  async function load() {
    const captured = seriesId, version = ++generation;
    opened = true; loading = true; error = ''; result = null; folder = null; selected = new Set();
    const [files, folders] = await Promise.all([getTvRenamePreview(captured), getFolderRenamePreview('tv', [captured])]);
    if (!alive || version !== generation || captured !== seriesId) return;
    loading = false;
    if (!files.ok) { error = files.error; return; }
    result = files.data;
    folder = folders.ok ? folders.data : null;
    selected = new Set(changeIds(files.data.items.map(i => ({ id: i.episode_file_id, status: i.status }))));
  }
  async function close() {
    const version = ++generation;
    opened = false; loading = false; error = ''; result = null; folder = null; selected = new Set();
    await tick();
    if (alive && generation === version) previewButton?.focus();
  }
  function toggle(id: number) {
    const next = new Set(selected);
    if (next.has(id)) next.delete(id); else next.add(id);
    selected = next;
  }
  async function execute(name: 'rename_files' | 'rename_series' | 'rename_series_folder', fileIds: number[]) {
    if (!result || running) return;
    const captured = seriesId, revision = result.naming_revision;
    running = true; runError = ''; command = null;
    const outcome = await runRename({
      name,
      target: { media_type: 'tv', series_ids: [captured], movie_ids: [], file_ids: name === 'rename_files' ? fileIds : [] },
      expected_naming_revision: revision,
      priority: 'normal',
    }, c => { if (alive && captured === seriesId) command = c; }, () => alive && captured === seriesId);
    if (!alive || captured !== seriesId) return;
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
  onDestroy(() => { alive = false; ++generation; });
</script>

<section aria-label="Series rename" aria-busy={loading || running}>
  <h2>Rename files</h2>
  <button bind:this={previewButton} type="button" disabled={loading || running} onclick={() => void load()}>{opened ? 'Reload rename preview' : 'Preview renames'}</button>
  {#if opened}<button type="button" disabled={running} onclick={close}>Close rename preview</button>{/if}
  <p class="muted">Renaming moves files on disk together with their subtitle and NFO files. Destination collisions are checked when you preview and again when the rename runs.</p>
  {#if loading}
    <p role="status">Loading rename preview…</p>
  {:else if error}
    <p class="error" role="alert">{error}</p>
  {:else if result}
    <p>Naming revision {result.naming_revision}. Paths are relative to this series’ folder.</p>
    {#if !result.rename_enabled}<p>Renaming is disabled in naming settings. Names are only cleaned up; season folders still follow the series settings.</p>{/if}
    <p>{result.files_considered} files considered; {result.unchanged_count} unchanged; {result.unavailable_count} unavailable; {result.blocked_count} blocked.</p>
    {#if result.files_considered === 0}<p>No episode files to preview.</p>
    {:else if result.items.length === 0}<p>No filename changes.</p>{/if}
    {#if result.items.length}
      <ul>
        {#each result.items as item (item.episode_file_id)}
          <li>
            {#if item.status === 'change'}
              <label><input type="checkbox" checked={selected.has(item.episode_file_id)} disabled={running} onchange={() => toggle(item.episode_file_id)} /> File {item.episode_file_id} · Season {item.season_number ?? '?'} · Episode {item.episode_numbers.join(', ')}</label>
            {:else}
              <p>File {item.episode_file_id} · {item.status === 'blocked' ? 'Blocked' : 'Unavailable'}</p>
            {/if}
            <dl><dt>Existing path</dt><dd>{item.existing_path ?? 'Unavailable'}</dd><dt>Proposed path</dt><dd>{item.new_path ?? 'Unavailable'}</dd></dl>
            {#each item.reasons as reason}<p class="reason">{reasonText[reason]}</p>{/each}
          </li>
        {/each}
      </ul>
      <div class="actions">
        <button type="button" disabled={running || picked.length === 0 || picked.length > 200} onclick={() => void execute('rename_files', picked)}>Rename selected ({picked.length})</button>
        <button type="button" disabled={running || changeIds(rows).length === 0} onclick={() => void execute('rename_series', [])}>Rename all</button>
      </div>
    {/if}
    {#if folder}
      <h3>Series folder</h3>
      {#each folder.items as f (f.series_id)}
        <dl><dt>Existing folder</dt><dd>{f.existing_path}</dd><dt>Proposed folder</dt><dd>{f.new_path ?? 'Unavailable'}</dd></dl>
        {#each f.reasons as reason}<p class="reason">{reasonText[reason]}</p>{/each}
        {#if f.status === 'change'}<button type="button" disabled={running} onclick={() => void execute('rename_series_folder', [])}>Rename series folder</button>{/if}
      {:else}
        <p>The series folder already matches its naming format.</p>
      {/each}
    {/if}
  {/if}
  {#if command}
    <p role="status">{summarize(command as RenameCommandDetail)}</p>
    {#if running && !isTerminal(command)}<button type="button" onclick={() => void cancel()}>Cancel rename</button>{/if}
    {#if 'items' in command}{#each command.items.filter(i => i.state !== 'committed') as item}<p class="reason">Not renamed: {item.old_path}{item.error_code ? ` (${item.error_code})` : ''}</p>{/each}{/if}
  {/if}
  {#if runError}<p class="error" role="alert">{runError}</p>{/if}
</section>

<style>
  section { margin-top: 1.5rem; }
  li, .reason { overflow-wrap: anywhere; }
  ul { padding-inline-start: 1.3rem; }
  li { margin-block: 1rem; }
  .actions { display: flex; gap: .5rem; flex-wrap: wrap; }
</style>
