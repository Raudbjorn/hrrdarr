<script lang="ts">
  import { onDestroy, untrack } from 'svelte';
  import type { ApiPage, FilePatch, FileResource, MediaDomain } from './api.generated';
  import type { SettingsWriteState } from './api';
  import { listMediaFiles, readMediaFiles, updateMediaFile, editMediaFiles, listQualityDefinitions, getCustomFormatSchema } from './api';
  import { emptyFileMetadataDraft, fileMetadataCatalogs, buildFileMetadataPatch, validateFileReadback, mixedFileValue, fileValueLabel, type FileMetadataCatalogs } from './file-metadata-draft';

  type Owner = { domain: MediaDomain; id: number; title: string };
  type Action = 'keep' | 'clear' | 'set';
  type Field = 'quality' | 'languages' | 'release_group' | 'edition' | 'indexer_flags' | 'release_type';
  let { target, active = true, relatedWrite = 'idle', discardVersion = 0, refreshVersion = 0, onwrite = () => {}, ondirty = () => {}, onsaved = () => {} }: {
    target: Owner | null; active?: boolean; relatedWrite?: SettingsWriteState; discardVersion?: number; refreshVersion?: number;
    onwrite?: (state: SettingsWriteState) => void; ondirty?: (dirty: boolean) => void;
    onsaved?: (target: { domain: MediaDomain; id: number }) => void;
  } = $props();

  const freshActions = (): Record<Field, Action> => ({ quality: 'keep', languages: 'keep', release_group: 'keep', edition: 'keep', indexer_flags: 'keep', release_type: 'keep' });
  let owner = $state<Owner | null>(null), page = $state<ApiPage<FileResource> | null>(null), catalogs = $state<FileMetadataCatalogs | null>(null);
  let offset = $state(0), selected = $state<number[]>([]), editing = $state<FileResource[]>([]), inspected = $state<FileResource | null>(null);
  let actions = $state(freshActions()), qualityId = $state(''), revisionMode = $state<'unknown' | 'known'>('unknown');
  let revisionVersion = $state(''), revisionReal = $state(''), repack = $state(false);
  let languageIds = $state<number[]>([]), group = $state(''), edition = $state(''), flags = $state<number[]>([]), releaseType = $state('');
  let loading = $state(false), busy = $state(false), uncertain = $state(false), error = $state(''), notice = $state(''), contextError = $state('');
  let attempted = $state<{ ids: number[]; patch: FilePatch } | null>(null), reconciled = $state(false);
  let seenRefresh = $state(0);
  const stale = $derived(refreshVersion !== seenRefresh);
  let alive = true, epoch = 0, seenDiscard = -1, wasActive = false;
  const dirty = $derived(Object.values(actions).some(action => action !== 'keep'));
  const locked = $derived(loading || busy || uncertain || relatedWrite !== 'idle' || !active || !!contextError);
  const fields = $derived<{ key: Field; label: string }[]>([
    { key: 'quality', label: 'Quality' }, { key: 'languages', label: 'Languages' },
    { key: 'release_group', label: 'Release group' }, { key: 'indexer_flags', label: 'Indexer flags' },
    ...(owner?.domain === 'tv' ? [{ key: 'release_type' as const, label: 'Release type' }] : [{ key: 'edition' as const, label: 'Edition' }]),
  ]);
  $effect(() => onwrite(busy ? 'busy' : uncertain ? 'uncertain' : 'idle'));
  $effect(() => ondirty(dirty));
  $effect(() => {
    if (stale && owner && same(owner, target) && active && !editing.length && !dirty && !loading && !busy && !uncertain && relatedWrite === 'idle' && !error)
      untrack(() => { void load(); });
  });
  const same = (a: Owner | null, b: Owner | null) => !!a && !!b && a.domain === b.domain && a.id === b.id;
  function current(token: number, captured: Owner) { return alive && token === epoch && same(owner, captured); }
  function clearDraft() {
    editing = []; actions = freshActions(); attempted = null; reconciled = false;
    qualityId = ''; revisionMode = 'unknown'; revisionVersion = ''; revisionReal = ''; repack = false;
    languageIds = []; group = ''; edition = ''; flags = []; releaseType = '';
  }
  function discard(): boolean { return !dirty || window.confirm('Discard unsaved file metadata changes?'); }

  // Parent admission owns changing the library selection. Null during a parent refresh is not a reset.
  $effect(() => {
    const next = target, shown = active, version = discardVersion;
    untrack(() => {
      if (next && !same(owner, next)) {
        if (busy || uncertain || (dirty && version === seenDiscard)) {
          contextError = 'Finish or reconcile the retained file edit before changing its library owner.';
          return;
        }
        ++epoch; owner = { ...next }; offset = 0; page = null; catalogs = null; selected = []; inspected = null;
        loading = false; clearDraft(); error = ''; notice = ''; contextError = ''; seenDiscard = version;
        wasActive = shown; if (shown) void load(); return;
      }
      if (next && same(owner, next)) { owner = { ...next }; contextError = ''; }
      if (version !== seenDiscard && !busy && !uncertain) { seenDiscard = version; clearDraft(); selected = []; }
      if (shown && !wasActive && owner && !dirty && !busy && !uncertain) void load();
      wasActive = shown;
    });
  });

  async function load() {
    if (!owner || loading || busy || uncertain || relatedWrite !== 'idle' || !active) return;
    const captured = { ...owner }, token = ++epoch, revision = refreshVersion; loading = true; error = '';
    const [files, definitions, schema] = await Promise.all([listMediaFiles(captured.domain, captured.id, offset), listQualityDefinitions(captured.domain), getCustomFormatSchema(captured.domain)]);
    if (!current(token, captured)) return;
    loading = false;
    if (!files.ok || !definitions.ok || !schema.ok) {
      error = [files, definitions, schema].flatMap(result => result.ok ? [] : [result.error]).join(' '); catalogs = null; return;
    }
    const choices = fileMetadataCatalogs(captured.domain, definitions.data, schema.data);
    const items = files.data?.items;
    if (!Array.isArray(items) || items.length > 25 || files.data.offset !== offset || files.data.limit !== 25 || !Number.isSafeInteger(files.data.total) || files.data.total < items.length) {
      error = 'File listing returned an invalid page. Reload before editing.'; catalogs = null; return;
    }
    const checked = items.length ? validateFileReadback(captured.domain, captured.id, items.map(item => item.id), items) : { ok: true as const, data: [] as FileResource[] };
    if (!choices.ok || !checked.ok) { error = !choices.ok ? choices.error : !checked.ok ? checked.error : ''; catalogs = null; return; }
    catalogs = choices.data; page = { ...files.data, items: checked.data }; seenRefresh = revision;
    selected = selected.filter(id => checked.data.some(file => file.id === id));
    if (inspected) inspected = checked.data.find(file => file.id === inspected?.id) ?? null;
  }
  async function refresh() {
    if (uncertain) { await reconcile(); return; }
    if (locked || !discard()) return;
    clearDraft(); await load();
  }
  async function changePage(next: number) {
    if (locked || !discard()) return;
    clearDraft(); selected = []; inspected = null; offset = next; await load();
  }
  function begin(files: FileResource[]) {
    if (locked || !catalogs || !files.length || !discard()) return;
    clearDraft(); editing = structuredClone($state.snapshot(files)); error = ''; notice = '';
    // Single-file values are convenience prefills only: every field still starts at Keep unchanged.
    if (files.length === 1) {
      const file = files[0]; qualityId = file.quality ? String(file.quality.quality_id) : '';
      if (file.quality?.revision) { revisionMode = 'known'; revisionVersion = String(file.quality.revision.version); revisionReal = String(file.quality.revision.real); repack = file.quality.revision.is_repack; }
      languageIds = [...(file.languages ?? [])]; group = file.release_group ?? '';
      edition = 'edition' in file ? file.edition ?? '' : '';
      releaseType = 'release_type' in file && file.release_type !== null ? String(file.release_type) : '';
      flags = catalogs.indexer_flags.filter(choice => ((file.indexer_flags ?? 0) & choice.value) !== 0).map(choice => choice.value);
    }
  }
  function draftPatch() {
    const draft = emptyFileMetadataDraft();
    const number = (text: string) => text.trim() === '' ? NaN : Number(text);
    const values = {
      quality: { quality_id: number(qualityId), revision: revisionMode === 'unknown' ? null : { version: number(revisionVersion), real: number(revisionReal), is_repack: repack } },
      languages: [...languageIds], release_group: group, edition,
      indexer_flags: flags.reduce((mask, flag) => mask | flag, 0), release_type: number(releaseType),
    };
    draft.quality = actions.quality === 'set' ? { action: 'set', value: values.quality } : { action: actions.quality };
    draft.languages = actions.languages === 'set' ? { action: 'set', value: values.languages } : { action: actions.languages };
    draft.release_group = actions.release_group === 'set' ? { action: 'set', value: group } : { action: actions.release_group };
    draft.indexer_flags = actions.indexer_flags === 'set' ? { action: 'set', value: values.indexer_flags } : { action: actions.indexer_flags };
    if (owner?.domain === 'movies') draft.edition = actions.edition === 'set' ? { action: 'set', value: edition } : { action: actions.edition };
    else draft.release_type = actions.release_type === 'set' ? { action: 'set', value: values.release_type } : { action: actions.release_type };
    return buildFileMetadataPatch(owner!.domain, draft, catalogs!);
  }
  function merge(files: FileResource[]) {
    if (page) page = { ...page, items: page.items.map(file => files.find(saved => saved.id === file.id) ?? file) };
    if (inspected) inspected = files.find(file => file.id === inspected?.id) ?? inspected;
  }
  async function save() {
    if (!owner || !catalogs || locked || !editing.length || !dirty || reconciled) return;
    const patch = draftPatch(); if (!patch.ok) { error = patch.error; return; }
    const captured = { ...owner }, token = ++epoch, ids = editing.map(file => file.id);
    attempted = { ids: [...ids], patch: structuredClone(patch.data) }; busy = true; error = ''; notice = '';
    const result = ids.length === 1 ? await updateMediaFile(captured.domain, ids[0], patch.data) : await editMediaFiles(captured.domain, ids, patch.data);
    if (!current(token, captured)) return;
    if (!result.ok) {
      error = result.error; uncertain = result.status === undefined || result.status >= 500; busy = false; return;
    }
    const response = validateFileReadback(captured.domain, captured.id, ids, ids.length === 1 ? [result.data] : result.data);
    if (!response.ok) { error = response.error; uncertain = true; busy = false; return; }
    // A successful mutation payload does not replace the explicit read of every attempted ID.
    const readback = await readMediaFiles(captured.domain, captured.id, ids);
    if (!current(token, captured)) return;
    busy = false;
    if (!readback.ok) { error = `Metadata was saved, but complete readback failed. ${readback.error}`; uncertain = true; return; }
    merge(readback.data); clearDraft(); notice = `Saved and read back metadata for ${ids.length} file${ids.length === 1 ? '' : 's'}.`; onsaved(captured);
  }
  async function reconcile() {
    if (!owner || !attempted || busy || loading || relatedWrite !== 'idle' || !active) return;
    const captured = { ...owner }, token = ++epoch; busy = true; error = '';
    const result = await readMediaFiles(captured.domain, captured.id, attempted.ids);
    if (!current(token, captured)) return;
    busy = false;
    if (!result.ok) { error = result.error; return; }
    merge(result.data); uncertain = false; reconciled = true;
    notice = 'Every attempted file was read back. Compare the current values below with your retained edit, then finish this review before editing again. Readback does not identify which writer made a change.';
    onsaved(captured);
  }
  function finishReview() {
    if (locked || !reconciled) return;
    clearDraft(); notice = 'Current metadata retained. Select files to start a new edit.';
  }
  function closeEdit() { if (!locked && discard()) { clearDraft(); error = ''; } }
  function shownValue(files: FileResource[], field: Field) {
    const value = mixedFileValue(files, field);
    if (value.kind === 'mixed') return 'Mixed values';
    const file = files[0];
    if (!file) return 'Unknown';
    if (field === 'languages') return languageLabel(file.languages);
    if (field === 'quality' && file.quality) {
      const revision = file.quality.revision;
      return `${qualityLabel(file)}; ${revision ? `version ${revision.version}, real ${revision.real}${revision.is_repack ? ', repack' : ''}` : 'revision unknown'}`;
    }
    if (field === 'indexer_flags' && file.indexer_flags !== null) {
      if (file.indexer_flags === 0) return 'Known no flags';
      const choices = catalogs?.indexer_flags ?? [], mask = choices.reduce((bits, choice) => bits | choice.value, 0);
      if (file.indexer_flags > mask || (file.indexer_flags & ~mask) !== 0) return `Unrecognized flag mask ${file.indexer_flags}`;
      return choices.filter(choice => (file.indexer_flags! & choice.value) !== 0).map(choice => choice.label).join(', ');
    }
    if (field === 'release_type' && 'release_type' in file && file.release_type !== null) return catalogs?.release_types.find(choice => choice.value === file.release_type)?.label ?? `Unrecognized release type ${file.release_type}`;
    return fileValueLabel(value.value);
  }
  function qualityLabel(file: FileResource) {
    if (!file.quality) return 'Unknown';
    return catalogs?.qualities.find(choice => choice.value === file.quality!.quality_id)?.label ?? `Unrecognized quality ${file.quality.quality_id}`;
  }
  const languageLabel = (ids: number[] | null) => ids === null ? 'Unknown' : ids.length === 0 ? 'Known empty' : ids.map(id => catalogs?.languages.find(choice => choice.value === id)?.label ?? `Unrecognized language ${id}`).join(', ');
  onDestroy(() => { alive = false; ++epoch; });
</script>

<svelte:window onbeforeunload={event => { if (dirty || busy || uncertain) { event.preventDefault(); event.returnValue = ''; } }} />

<section aria-label="File metadata" aria-busy={loading || busy}>
  {#if stale}<p role="status">An import completed. Your retained edit is unchanged; the file listing will refresh when the edit and any pending write are resolved.</p>{/if}
  <h2>Files and metadata</h2>
  {#if owner}
    <p>{owner.title} · {owner.domain === 'tv' ? 'TV series' : 'Movie'} {owner.id}</p>
    <p>Inspect stored file facts and correct metadata. These edits do not move or rename files.</p>
    {#if loading}<p role="status">Loading file metadata and choices…</p>{/if}
    {#if busy}<p role="status">Saving or reading back file metadata…</p>{/if}
    {#if error}<p role="alert">{error}</p>{/if}
    {#if contextError}<p role="alert">{contextError}</p>{/if}
    {#if notice}<p role="status">{notice}</p>{/if}
    {#if relatedWrite !== 'idle'}<p role="status">Finish or reconcile the related library write before editing files.</p>{/if}
    {#if uncertain}<p role="alert">The write outcome has not been fully reconciled. Read back every attempted file before another edit. Missing files keep this recovery state open.</p>{/if}
    <button type="button" disabled={loading || busy || relatedWrite !== 'idle' || !active} onclick={refresh}>{uncertain ? 'Read back attempted files' : 'Reload file metadata'}</button>
    {#if page}
      <p>{page.total} files. Selection applies only to this page.</p>
      <div class="scroll"><table><thead><tr><th scope="col">Select</th><th scope="col">File</th><th scope="col">Stored quality / languages</th><th scope="col">Actions</th></tr></thead><tbody>
        {#each page.items as file (file.id)}<tr>
          <td><input type="checkbox" aria-label={`Select file ${file.id}`} value={file.id} bind:group={selected} disabled={locked || editing.length > 0} /></td>
          <td>File {file.id}<span class="path">{file.relative_path ?? file.path}</span><span>{file.size === null ? 'Size unknown' : `${file.size} stored bytes`}</span></td>
          <td>{qualityLabel(file)}<br />{languageLabel(file.languages)}</td>
          <td><button type="button" onclick={() => inspected = file}>Inspect file {file.id}</button><button type="button" disabled={locked || !catalogs} onclick={() => begin([file])}>Edit file {file.id}</button></td>
        </tr>{:else}<tr><td colspan="4">No file records for this library item.</td></tr>{/each}
      </tbody></table></div>
      <div class="actions"><button type="button" disabled={locked || !catalogs || !selected.length} onclick={() => begin(page!.items.filter(file => selected.includes(file.id)))}>Edit selected files ({selected.length})</button>
        <button type="button" disabled={locked || offset === 0} onclick={() => changePage(Math.max(0, offset - 25))}>Previous files</button>
        <button type="button" disabled={locked || offset + 25 >= page.total} onclick={() => changePage(offset + 25)}>Next files</button></div>
    {/if}
    {#if inspected}
      <section aria-label={`File ${inspected.id} facts`}><h3>File {inspected.id} facts</h3>
        <dl><dt>Path</dt><dd>{inspected.path}</dd><dt>Relative path</dt><dd>{inspected.relative_path ?? 'Unavailable'}</dd><dt>Added</dt><dd>{inspected.date_added ?? 'Unknown'}</dd>
          {#if 'season_number' in inspected}<dt>Season</dt><dd>{inspected.season_number ?? 'Unknown'}</dd>{/if}
          {#if 'original_file_path' in inspected}<dt>Original path</dt><dd>{inspected.original_file_path ?? 'Unknown'}</dd>{/if}
          {#each fields as field}<dt>{field.label}</dt><dd>{shownValue([inspected], field.key)}</dd>{/each}
          <dt>Scene name / custom-format score / cutoff</dt><dd>Unavailable in current file metadata</dd>
        </dl>
        {#if inspected.media_info}<details><summary>Stored media information</summary><dl>{#each Object.entries(inspected.media_info) as [name, value]}<dt>{name.replaceAll('_', ' ')}</dt><dd>{fileValueLabel(value)}</dd>{/each}</dl></details>{:else}<p>Media information is unknown.</p>{/if}
        <button type="button" onclick={() => inspected = null}>Close file facts</button>
      </section>
    {/if}
    {#if editing.length && catalogs}
      <form onsubmit={event => { event.preventDefault(); void save(); }} aria-label="Edit file metadata">
        <h3>Edit {editing.length === 1 ? `file ${editing[0].id}` : `${editing.length} selected files`}</h3>
        <p>Keep unchanged preserves each file's value. Clear records an unknown value. Set replaces the field for every selected file.</p>
        <p>Only explicitly changed fields are sent. Concurrent edits by another client are not detected by this API.</p>
        <fieldset disabled={locked || reconciled}><legend>Metadata changes</legend>
          {#each fields as field}
            <div class="field"><label>{field.label} action<select aria-label={`${field.label} action`} bind:value={actions[field.key]}><option value="keep">Keep unchanged</option><option value="clear">Clear to unknown</option><option value="set">Set value</option></select></label>
              <p>Before edit: {shownValue(editing, field.key)}</p>
              {#if actions[field.key] === 'set'}
                {#if field.key === 'quality'}
                  <label>Quality<select bind:value={qualityId}><option value="">Choose a quality</option>{#if qualityId && !catalogs.qualities.some(choice => String(choice.value) === qualityId)}<option value={qualityId}>Unrecognized quality {qualityId}</option>{/if}{#each catalogs.qualities as choice}<option value={String(choice.value)}>{choice.label}</option>{/each}</select></label>
                  <label>Quality revision<select bind:value={revisionMode}><option value="unknown">Replace revision with unknown</option><option value="known">Set complete revision</option></select></label>
                  {#if revisionMode === 'known'}<label>Revision version<input inputmode="numeric" bind:value={revisionVersion} /></label><label>Revision real<input inputmode="numeric" bind:value={revisionReal} /></label><label class="check"><input type="checkbox" bind:checked={repack} />Repack</label>{/if}
                {:else if field.key === 'languages'}
                  <fieldset><legend>Concrete languages</legend><div class="choices">{#each catalogs.languages as choice}<label class="check"><input type="checkbox" value={choice.value} bind:group={languageIds} />{choice.label}</label>{/each}</div>{#if languageIds.some(id => !catalogs!.languages.some(choice => choice.value === id))}<p role="alert">The retained language selection contains unsupported values. Clear the selection and choose concrete languages before saving this field.</p>{/if}<button type="button" onclick={() => languageIds = []}>Clear language selection</button><p>No selected languages sets a known empty list. Unknown language is a concrete entry, distinct from clearing the field.</p></fieldset>
                {:else if field.key === 'release_group'}<label>Release group<input bind:value={group} /></label><p>An empty value is saved as an empty string.</p>
                {:else if field.key === 'edition'}<label>Edition<input bind:value={edition} /></label><p>An empty value is saved as an empty string.</p>
                {:else if field.key === 'indexer_flags'}<fieldset><legend>Indexer flags</legend><div class="choices">{#each catalogs.indexer_flags as choice}<label class="check"><input type="checkbox" value={choice.value} bind:group={flags} />{choice.label}</label>{/each}</div><p>No selected flags records known no flags.</p></fieldset>
                {:else if field.key === 'release_type'}<label>Release type<select bind:value={releaseType}><option value="">Choose a release type</option>{#each catalogs.release_types as choice}<option value={String(choice.value)}>{choice.label}</option>{/each}</select></label>{/if}
              {/if}
            </div>
          {/each}
          <button type="submit" disabled={!dirty}>Save file metadata</button>
        </fieldset>
        {#if reconciled}
          <section aria-label="Current file metadata readback"><h4>Current values after readback</h4>
            {#each page?.items.filter(file => attempted?.ids.includes(file.id)) ?? [] as file (file.id)}<h5>File {file.id}</h5><dl>{#each fields as field}<dt>{field.label}</dt><dd>{shownValue([file], field.key)}</dd>{/each}</dl>{/each}
          </section><button type="button" disabled={locked} onclick={finishReview}>Finish readback review</button>
        {:else}<button type="button" disabled={locked} onclick={closeEdit}>Close metadata edit</button>{/if}
      </form>
    {/if}
  {:else}<p>Select a library item to inspect its files.</p>{/if}
</section>

<style>
  section{margin-block:1.5rem}h2,h3{overflow-wrap:anywhere}.scroll{overflow-x:auto}table{border-collapse:collapse;width:100%}th,td{text-align:left;vertical-align:top;padding:.65rem;border-bottom:1px solid #cbd5de}td .path{display:block;overflow-wrap:anywhere;max-width:28rem}td button{display:block;margin-bottom:.4rem}.actions{display:flex;gap:.6rem;flex-wrap:wrap;margin-block:.8rem}fieldset{min-width:0;border:1px solid #cbd5de;padding:1rem;margin-block:1rem}.field{padding-block:.75rem;border-bottom:1px solid #cbd5de}label{display:grid;gap:.35rem;margin-block:.5rem}.check{display:flex;align-items:center;gap:.5rem}.choices{display:grid;grid-template-columns:repeat(auto-fit,minmax(11rem,1fr));gap:.2rem .8rem}input:not([type=checkbox]),select{width:100%;max-width:32rem;min-width:0;box-sizing:border-box}dl{display:grid;grid-template-columns:minmax(8rem,1fr) 3fr;gap:.5rem}dd{margin:0;overflow-wrap:anywhere;white-space:pre-wrap}p{overflow-wrap:anywhere}[role=alert]{border-left:3px solid #a12637;padding-left:.75rem;color:#a12637}button,select{min-height:2.5rem}@media(max-width:600px){dl{grid-template-columns:1fr}dd{margin-bottom:.5rem}fieldset{padding:.65rem}.choices{grid-template-columns:1fr}}
</style>
