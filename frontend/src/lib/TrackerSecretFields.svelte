<script lang="ts">
  // Write-only credentials of a tracker indexer. Nothing here is ever read back from the server.
  import type {TrackerInfo, TrackerSecrets} from './provider-draft';
  let {info, secrets = $bindable(), tv, movies}: {info: TrackerInfo; secrets: TrackerSecrets; tv: boolean; movies: boolean} = $props();
</script>
{#if info.credential === 'none'}
  <p>{info.label} needs no credentials. Replacing removes any stored bundle.</p>
{:else if info.credential === 'api_key'}
  <label>{info.secretLabels[0]}<input type="password" bind:value={secrets.apiKey} autocomplete="new-password" required maxlength="4096" /></label>
{:else if info.credential === 'account'}
  <label>{info.secretLabels[0]}<input bind:value={secrets.username} autocomplete="off" required maxlength="4096" /></label>
  <label>{info.secretLabels[1]}<input type="password" bind:value={secrets.password} autocomplete="new-password" required maxlength="4096" /></label>
{:else}
  <label>Cookie (optional)<input type="password" bind:value={secrets.cookie} autocomplete="new-password" maxlength="4096" placeholder="uid=…; pass=…" /></label>
  {#if tv}<label>TV feed parameters (name=value per line)<textarea bind:value={secrets.tvParameters} rows="4" autocomplete="off" spellcheck="false"></textarea></label>{/if}
  {#if movies}<label>Movie feed parameters (name=value per line)<textarea bind:value={secrets.movieParameters} rows="4" autocomplete="off" spellcheck="false"></textarea></label>{/if}
{/if}
