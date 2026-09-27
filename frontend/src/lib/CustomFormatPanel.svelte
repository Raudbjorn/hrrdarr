<script lang="ts">
  import { onMount } from 'svelte';
  import { parseCustomFormatDraft, parseCommunityCustomFormat, exportCommunityCustomFormat } from './custom-format-draft';
  import type { CustomFormat, CustomFormatInput, CustomFormatSchema, CustomFormatSpecification, MediaDomain } from './api.generated';
  import { listCustomFormats, getCustomFormatSchema, createCustomFormat, updateCustomFormat, deleteCustomFormat } from './api';
  let domain: MediaDomain = $state('tv');
  let formats: CustomFormat[] = $state([]), schema: CustomFormatSchema | null = $state(null);
  let draft: CustomFormatInput | null = $state(null), selected: number | null = $state(null);
  let loading = $state(false), busy = $state(false), uncertain = $state(false), confirming = $state(false);
  let jsonText = $state('');
  let jsonFormat = $state<'native' | 'community'>('native');
  let error = $state(''), notice = $state(''), kind = $state('release_title');
  let alive = true, generation = 0;
  const label = (kind: string) => kind.replaceAll('_', ' ');
  const copy = <T,>(value: T): T => JSON.parse(JSON.stringify(value));
  async function load() {
    const version = ++generation, scope = domain;
    loading = true; error = ''; schema = null; formats = []; draft = null; selected = null; confirming = false;
    const [rows, contract] = await Promise.all([listCustomFormats(scope), getCustomFormatSchema(scope)]);
    if (!alive || version !== generation || scope !== domain) return false;
    loading = false;
    if (!rows.ok || !contract.ok) { error = !rows.ok ? rows.error : !contract.ok ? contract.error : ''; return false; }
    formats = rows.data; schema = contract.data; uncertain = false;
    if (!schema.conditions.some(c => c.kind === kind)) kind = schema.conditions[0]?.kind ?? 'release_title';
    return true;
  }
  function changeDomain(next: MediaDomain) {
    if (busy || next === domain) return;
    domain = next; notice = ''; jsonText = ''; void load();
  }
  function edit(format?: CustomFormat) {
    selected = format?.id ?? null;
    draft = format ? copy({ name: format.name, include_when_renaming: format.include_when_renaming, specifications: format.specifications }) : { name: '', include_when_renaming: false, specifications: [] };
    confirming = false; error = ''; notice = '';
  }
  function add(specification?: CustomFormatSpecification) {
    if (!draft || !schema || draft.specifications.length >= schema.max_specifications) return;
    const condition = schema.conditions.find(c => c.kind === kind);
    if (!condition) return;
    draft.specifications.push(copy(specification ?? { name: label(kind), negate: false, required: false, condition }));
  }
  async function save() {
    if (!draft || busy || uncertain) return;
    const checked = schema && parseCustomFormatDraft(JSON.stringify(draft), schema);
    if (!checked || !checked.ok) { error = checked ? checked.error : 'Reload the condition schema.'; return; }
    const scope = domain, version = generation;
    busy = true; error = ''; notice = '';
    const result = selected === null ? await createCustomFormat(scope, copy(draft)) : await updateCustomFormat(scope, selected, copy(draft));
    if (!alive || version !== generation) return;
    if (!result.ok) { busy = false; error = result.error; uncertain = result.status === undefined || result.status >= 500; return; }
    const refreshed = await load();
    if (!alive) return;
    busy = false;
    if (refreshed) edit(result.data);
    notice = refreshed ? 'Custom format saved.' : 'Saved, but refreshing failed. Reload to continue.';
  }
  function applyJson() {
    if (!schema) return;
    const result = jsonFormat === 'community' ? parseCommunityCustomFormat(jsonText, schema) : parseCustomFormatDraft(jsonText, schema);
    if (!result.ok) { error = result.error; return; }
    draft = result.data; selected = null; confirming = false; error = ''; notice = 'JSON applied to a new draft. Review it before saving.';
  }
  function exportJson() {
    if (!draft || !schema) return;
    if (jsonFormat === 'native') { jsonText = JSON.stringify(draft, null, 2); error = ''; return; }
    const result = exportCommunityCustomFormat(copy(draft), schema);
    if (!result.ok) { error = result.error; return; }
    jsonText = result.data; error = '';
  }
  function cloneFormat() {
    if (!draft) return;
    draft = copy(draft); selected = null; confirming = false; notice = 'Format copied. Choose a unique name before saving.';
  }
  async function remove() {
    if (selected === null || busy || !confirming || uncertain) return;
    const version = generation;
    busy = true; error = '';
    const result = await deleteCustomFormat(domain, selected);
    if (!alive || version !== generation) return;
    if (!result.ok) { busy = false; error = result.error; uncertain = result.status === undefined || result.status >= 500; return; }
    const refreshed = await load();
    if (!alive) return;
    busy = false;
    notice = refreshed ? 'Custom format deleted.' : 'Deleted, but refreshing failed. Reload to continue.';
  }
  onMount(() => { void load(); return () => { alive = false; ++generation; }; });
</script>

<section aria-labelledby="custom-formats-heading">
  <h1 id="custom-formats-heading">Custom formats</h1>
  <nav aria-label="Custom format media type"><button disabled={busy} aria-pressed={domain === 'tv'} onclick={() => changeDomain('tv')}>TV</button><button disabled={busy} aria-pressed={domain === 'movies'} onclick={() => changeDomain('movies')}>Movies</button></nav>
  <p>Build matching conditions, then assign scores in a quality profile. Every condition kind needs a match, and all required conditions must match. Optional conditions of the same kind provide alternatives.</p>
  {#if error}<p role="alert">{error}</p>{/if}
  {#if uncertain}<p>The outcome is uncertain. Reload before making more changes.</p>{/if}
  {#if notice}<p role="status">{notice}</p>{/if}
  <button disabled={busy} onclick={() => { notice = ''; void load(); }}>Reload custom formats</button>
  {#if loading}<p role="status">Loading custom formats…</p>{:else if schema}
    {#if formats.length === 0}<p>No custom formats in this library.</p>{/if}
    <ul>{#each formats as format (format.id)}<li><button disabled={busy || uncertain} aria-pressed={selected === format.id} onclick={() => edit(format)}>{format.name}</button><span>{format.specifications.length} conditions</span></li>{/each}</ul>
    <button disabled={busy || uncertain || formats.length >= schema.max_formats} onclick={() => edit()}>New custom format</button>
    <details><summary>Import or export JSON</summary>
      <label>JSON format<select aria-label="JSON format" bind:value={jsonFormat} disabled={busy || uncertain}><option value="native">Native</option><option value="community">Sonarr / Radarr community</option></select></label>
      <label>Custom format JSON<textarea aria-label="Custom format JSON" rows="8" maxlength="73728" bind:value={jsonText} disabled={busy || uncertain}></textarea></label>
      <p>Choose the document format and library domain explicitly. Up to 72 KiB; unknown community conditions or options reject the entire document. Applying checks structure and creates a new draft; Save validates patterns before writing.</p>
      <button disabled={busy || uncertain} onclick={applyJson}>Apply JSON to new draft</button>
      <button disabled={busy || uncertain || !draft} onclick={exportJson}>Export draft as JSON</button>
    </details>
    {#if draft}
      <form onsubmit={(event) => { event.preventDefault(); void save(); }}>
        <fieldset disabled={busy || uncertain}>
          <legend>{selected === null ? 'New custom format' : 'Edit custom format'}</legend>
          <button type="button" onclick={cloneFormat}>Clone format</button>
          <label>Format name<input required bind:value={draft.name} /></label>
          <label><input type="checkbox" bind:checked={draft.include_when_renaming} /> Include when renaming</label>
          <p>Names support up to 100 characters. Renaming also requires a naming template that uses custom formats.</p>
          <label>Condition kind<select aria-label="Condition kind" bind:value={kind}>{#each schema.conditions as condition}<option value={condition.kind}>{label(condition.kind)}</option>{/each}</select></label>
          <button type="button" disabled={draft.specifications.length >= schema.max_specifications} onclick={() => add()}>Add condition</button>
          <label>Condition preset<select aria-label="Condition preset" value="" disabled={draft.specifications.length >= schema.max_specifications} onchange={(event) => { const preset = schema?.presets[kind]?.[Number(event.currentTarget.value)]; if (event.currentTarget.value !== '' && preset) add(preset.specification); event.currentTarget.value = ''; }}><option value="">Choose a preset to copy</option>{#each schema.presets[kind] ?? [] as preset, index}<option value={index}>{preset.label}</option>{/each}</select></label>
          {#if kind === 'release_title'}<p>Preferred Words is a starting example: replace “SPARKS” and “Framestor” with your own terms.</p>{/if}
          {#each draft.specifications as specification, index}
            <fieldset class="condition">
              <legend>Condition {index + 1}: {label(specification.condition.kind)}</legend>
              <label>Condition name<input required bind:value={specification.name} /></label>
              <label><input type="checkbox" bind:checked={specification.negate} /> Negate</label>
              <label><input type="checkbox" bind:checked={specification.required} /> Required</label>
              {#if 'pattern' in specification.condition}
                <label>Pattern<input required bind:value={specification.condition.pattern} /></label>
              {:else if specification.condition.kind === 'size'}
                <label>Minimum GiB<input type="number" min="0" step="any" required bind:value={specification.condition.min_gib} /></label>
                <label>Maximum GiB<input type="number" min="0" step="any" required bind:value={specification.condition.max_gib} /></label>
              {:else if specification.condition.kind === 'year'}
                <label>Minimum year<input type="number" step="1" required bind:value={specification.condition.min} /></label>
                <label>Maximum year<input type="number" step="1" required bind:value={specification.condition.max} /></label>
              {:else}
                <label>Value<select bind:value={specification.condition.value}>{#each schema.choices[specification.condition.kind] ?? [] as choice}<option value={choice.value}>{choice.label}</option>{/each}</select></label>
                {#if specification.condition.kind === 'language'}<label><input type="checkbox" bind:checked={specification.condition.except_language} /> Except language</label>{/if}
              {/if}
              <button type="button" disabled={draft.specifications.length >= schema.max_specifications} onclick={() => draft?.specifications.splice(index + 1, 0, copy(specification))}>Clone condition</button>
              <button type="button" onclick={() => draft?.specifications.splice(index, 1)}>Remove condition</button>
            </fieldset>
          {/each}
          <p>{draft.specifications.length} / {schema.max_specifications} conditions. Patterns are case insensitive and checked when saving.</p>
          <button disabled={draft.specifications.length === 0}>Save custom format</button>
          {#if selected !== null}<button type="button" onclick={() => confirming = true}>Delete custom format</button>{/if}
          {#if confirming}<p>Delete this format and its profile scores? This does not delete media.</p><button type="button" onclick={() => void remove()}>Confirm delete custom format</button><button type="button" onclick={() => confirming = false}>Cancel deletion</button>{/if}
        </fieldset>
      </form>
    {/if}
  {/if}
</section>
<style>
  section { max-width: 65rem; } nav, li { display: flex; gap: .75rem; align-items: center; }
  ul { padding-left: 1rem; } fieldset { margin: 1rem 0; padding: 1rem; border: 1px solid #8885; }
  label { display: block; margin: .65rem 0; } input:not([type=checkbox]), select { display: block; width: min(100%, 42rem); box-sizing: border-box; padding: .45rem; }
  button { margin: .25rem; padding: .45rem .7rem; } [role=alert] { color: #d84949; } .condition { background: #8881; }
</style>
