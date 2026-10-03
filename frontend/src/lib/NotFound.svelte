<script lang="ts">
  import { SETTINGS_INDEX_HASH, hashFor } from './routes';
  let { path }: { path: string } = $props();
</script>

<section role="alert" aria-labelledby="not-found-title">
  <h1 id="not-found-title">Page not found</h1>
  <p>No page exists at <code>{path}</code>.</p>
  <p><a href={hashFor('library')}>Go to Library</a> · <a href={SETTINGS_INDEX_HASH}>Go to Settings</a></p>
</section>
