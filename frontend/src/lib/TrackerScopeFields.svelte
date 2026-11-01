<script lang="ts">
  // One media scope of a tracker indexer: RSS/search policy, minimum seeders and (where the tracker has them) category ids.
  import type {TrackerDraft, TrackerInfo} from './provider-draft';
  let {label, info, draft = $bindable()}: {label: string; info: TrackerInfo; draft: TrackerDraft} = $props();
</script>
<label class="check"><input type="checkbox" bind:checked={draft.rss} />{label} RSS</label>
{#if info.searchable}
  <label class="check"><input type="checkbox" bind:checked={draft.automatic} />{label} automatic search</label>
  <label class="check"><input type="checkbox" bind:checked={draft.interactive} />{label} interactive search</label>
{:else}
  <p>{info.label} has no search API; only RSS sync is available.</p>
{/if}
<label>{label} minimum seeders<input inputmode="numeric" bind:value={draft.minimum} maxlength="7" placeholder="none" /></label>
{#if info.categorised}
  <label>{label} category ids<input bind:value={draft.categories} placeholder="leave empty for the tracker default" /></label>
{/if}
