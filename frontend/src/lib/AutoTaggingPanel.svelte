<script lang="ts">
  import { onMount } from 'svelte';
  import type { AutoTag, AutoTagSchemaEntry, MediaDomain, Tag } from './api.generated';
  import { listAutoTags, getAutoTagSchema, createAutoTag, updateAutoTag, deleteAutoTag, listTags } from './api';
  import { unknownTagWrite } from './tag-draft';
  import { emptyRule, newSpec, initFields, draftFromRule, toInput, summarizeSpecification, MAX_RULE_TAGS, MAX_SPECIFICATIONS, type RuleDraft, type SpecDraft } from './autotag-draft';

  let domain = $state<MediaDomain>('tv'), rules = $state<AutoTag[]>([]), tags = $state<Tag[]>([]), schema = $state<AutoTagSchemaEntry[]>([]);
  let loading = $state(false), loaded = $state(false), busy = $state(false), uncertain = $state(false);
  let error = $state(''), notice = $state('');
  let draft = $state<RuleDraft | null>(null), editingId = $state<number | null>(null), deleting = $state<AutoTag | null>(null);
  let epoch = 0, alive = true;
  const blocked = $derived(busy || loading || uncertain);
  const tagLabel = (id: number) => tags.find(t => t.id === id)?.label ?? `tag ${id}`;
  const entryFor = (impl: string) => schema.find(e => e.implementation === impl);

  async function load() {
    if (busy) return;
    const version = ++epoch, scope = domain;
    loading = true; loaded = false; error = ''; deleting = null;
    const [r, t, s] = await Promise.all([listAutoTags(scope), listTags(scope), getAutoTagSchema(scope)]);
    if (!alive || version !== epoch) return;
    loading = false;
    if (!r.ok) { error = r.error; return; }
    if (!t.ok) { error = t.error; return; }
    if (!s.ok) { error = s.error; return; }
    rules = r.data; tags = t.data; schema = s.data; loaded = true; uncertain = false;
  }
  function switchDomain(next: MediaDomain) {
    if (busy || uncertain || next === domain) return;
    domain = next; rules = []; tags = []; schema = []; draft = null; editingId = null; notice = '';
    void load();
  }
  function startCreate() {
    if (blocked || !loaded) return;
    notice = ''; error = ''; deleting = null; editingId = null;
    draft = emptyRule(); draft.specs.push(newSpec(schema[0]));
  }
  function startEdit(rule: AutoTag) {
    if (blocked || !loaded) return;
    notice = ''; error = ''; deleting = null; editingId = rule.id; draft = draftFromRule(rule, schema);
  }
  function cancel() { if (busy) return; draft = null; editingId = null; error = ''; }
  function addSpec() { if (draft && draft.specs.length < MAX_SPECIFICATIONS) draft.specs.push(newSpec(schema[0])); }
  function removeSpec(key: number) { if (draft) draft.specs = draft.specs.filter(s => s.key !== key); }
  function changeImplementation(spec: SpecDraft, impl: string) {
    const previous = entryFor(spec.implementation);
    if (!spec.name.trim() || spec.name === previous?.label) spec.name = entryFor(impl)?.label ?? spec.name;
    spec.implementation = impl; initFields(spec, entryFor(impl));
  }
  function toggleTag(id: number, on: boolean) {
    if (!draft) return;
    draft.tagIds = on ? [...new Set([...draft.tagIds, id])] : draft.tagIds.filter(t => t !== id);
  }
  async function save() {
    if (blocked || !loaded || !draft) return;
    const converted = toInput(draft, schema, tags);
    if (!converted.ok) { error = converted.error; return; }
    const scope = domain, id = editingId;
    busy = true; error = ''; notice = '';
    const result = id === null ? await createAutoTag(scope, converted.input) : await updateAutoTag(scope, id, converted.input);
    if (!alive) return;
    busy = false;
    if (scope !== domain) return;
    if (!result.ok) { error = result.error; uncertain = unknownTagWrite(result); return; }
    draft = null; editingId = null;
    await load();
    if (alive && scope === domain) notice = id === null ? 'Rule created.' : 'Rule saved.';
  }
  async function remove() {
    if (blocked || !deleting) return;
    const id = deleting.id, scope = domain;
    busy = true; error = ''; notice = '';
    const result = await deleteAutoTag(scope, id);
    if (!alive) return;
    busy = false; deleting = null;
    if (scope !== domain) return;
    if (!result.ok) { error = result.error; uncertain = unknownTagWrite(result); return; }
    await load();
    if (alive && scope === domain) notice = 'Rule deleted.';
  }
  onMount(() => { void load(); return () => { alive = false; ++epoch; }; });
</script>

<section aria-label="Auto tagging settings">
  <h1>Auto tagging</h1>
  <nav aria-label="Auto tagging media type">
    <button disabled={busy || uncertain} aria-pressed={domain === 'tv'} onclick={() => switchDomain('tv')}>TV</button>
    <button disabled={busy || uncertain} aria-pressed={domain === 'movies'} onclick={() => switchDomain('movies')}>Movies</button>
  </nav>
  <p>Rules add their tags to library items that match. A rule matches when every required specification matches and, if there are optional ones, at least one of them matches. Unknown facts never match, even when negated.</p>
  {#if error}<p role="alert">{error}</p>{/if}
  {#if notice}<p role="status">{notice}</p>{/if}
  {#if uncertain}<p role="alert">Write outcome unknown. Reload rules and review them before any further write.</p>{/if}
  {#if loading}<p role="status">Loading auto tagging rules…</p>{/if}
  {#if busy}<p role="status">Saving auto tagging change…</p>{/if}
  <p><button disabled={busy || loading} onclick={load}>Reload rules</button>
    <button disabled={blocked || !loaded || !!draft || tags.length === 0} onclick={startCreate}>New rule</button></p>
  {#if loaded && tags.length === 0}<p role="status">Create a tag for this media type first; rules need at least one tag.</p>{/if}

  {#if draft}
    <form aria-label={editingId === null ? 'Create rule' : 'Edit rule'} onsubmit={event => { event.preventDefault(); void save(); }}>
      <fieldset disabled={blocked}>
        <legend>{editingId === null ? 'Create rule' : `Edit rule ${editingId}`}</legend>
        <label>Rule name<input bind:value={draft.name} required maxlength="256" /></label>
        <label><input type="checkbox" bind:checked={draft.remove} /> Remove tags automatically when the rule stops matching</label>
        <fieldset>
          <legend>Tags (1 to {MAX_RULE_TAGS})</legend>
          {#each tags as tag (tag.id)}
            <label><input type="checkbox" checked={draft.tagIds.includes(tag.id)} onchange={event => toggleTag(tag.id, event.currentTarget.checked)} /> {tag.label}</label>
          {/each}
        </fieldset>
        {#each draft.specs as spec, index (spec.key)}
          {@const entry = entryFor(spec.implementation)}
          <fieldset aria-label={`Specification ${index + 1}`}>
            <legend>Specification {index + 1}</legend>
            <label>Specification name<input bind:value={spec.name} required maxlength="256" /></label>
            <label>Implementation
              <select value={spec.implementation} onchange={event => changeImplementation(spec, event.currentTarget.value)}>
                {#each schema as option (option.implementation)}<option value={option.implementation}>{option.label}</option>{/each}
                {#if !entry}<option value={spec.implementation}>{spec.implementation} (unavailable)</option>{/if}
              </select>
            </label>
            <label><input type="checkbox" bind:checked={spec.required} /> Required</label>
            <label><input type="checkbox" bind:checked={spec.negate} /> Negate</label>
            {#each entry?.fields ?? [] as field (field.name)}
              {#if field.kind === 'boolean'}
                <label><input type="checkbox" bind:checked={spec.flag[field.name]} /> {field.label}</label>
              {:else if field.kind === 'enum_list'}
                <fieldset><legend>{field.label}</legend>
                  {#each field.options as option (option)}
                    <label><input type="checkbox" value={option} bind:group={spec.pick[field.name]} /> {option}</label>
                  {/each}
                </fieldset>
              {:else if field.kind === 'text_list' || field.kind === 'integer_list'}
                <label>{field.label} (comma or newline separated)<textarea rows="2" bind:value={spec.text[field.name]}></textarea></label>
              {:else if field.kind === 'integer'}
                <label>{field.label}<input inputmode="numeric" bind:value={spec.text[field.name]} /></label>
              {:else}
                <label>{field.label}<input bind:value={spec.text[field.name]} /></label>
              {/if}
            {/each}
            <button type="button" disabled={draft.specs.length <= 1} onclick={() => removeSpec(spec.key)}>Remove specification {index + 1}</button>
          </fieldset>
        {/each}
        <button type="button" disabled={draft.specs.length >= MAX_SPECIFICATIONS} onclick={addSpec}>Add specification</button>
        <button type="submit">{editingId === null ? 'Create rule' : 'Save rule'}</button>
        <button type="button" disabled={busy} onclick={cancel}>Cancel</button>
      </fieldset>
    </form>
  {/if}

  {#if loaded}
    <ul aria-label="Auto tagging rules">
      {#each rules as rule (rule.id)}
        <li>
          <strong>{rule.name}</strong>
          · tags: {#each rule.tag_ids as id, i (id)}{i ? ', ' : ''}{tagLabel(id)}{/each}
          {#if rule.remove_tags_automatically}· removes tags automatically{/if}
          <ul>{#each rule.specifications as spec (spec.id)}<li>{summarizeSpecification(spec)}</li>{/each}</ul>
          <button disabled={blocked || !!draft} onclick={() => startEdit(rule)}>Edit {rule.name}</button>
          <button disabled={blocked} onclick={() => { deleting = rule; }}>Delete {rule.name}</button>
        </li>
      {:else}
        <li>No auto tagging rules in this media type.</li>
      {/each}
    </ul>
  {/if}
  {#if deleting}
    <div role="group" aria-label="Confirm rule deletion">
      <p>Delete rule “{deleting.name}”? Tags it already applied stay on library items.</p>
      <button disabled={blocked} onclick={remove}>Confirm delete rule</button>
      <button disabled={busy} onclick={() => { deleting = null; }}>Cancel rule deletion</button>
    </div>
  {/if}
</section>
