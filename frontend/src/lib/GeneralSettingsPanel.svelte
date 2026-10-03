<script lang="ts">
  import {onMount} from 'svelte';
  import {getHostSettings} from './api';
  import {describeHostResult,type GeneralSettingsState,type HostListView} from './general-settings';
  let model=$state<GeneralSettingsState|null>(null),loading=$state(false);
  let alive=true,epoch=0;
  async function load(){
    if(loading)return;
    loading=true;const version=++epoch;
    const result=await getHostSettings();
    if(!alive||version!==epoch)return;
    model=describeHostResult(result);loading=false;
  }
  onMount(()=>{void load();return()=>{alive=false;++epoch;};});
</script>
{#snippet list(title:string,item:HostListView)}
  <section aria-label={title}>
    <h3>{title}</h3>
    <p>{item.summary}</p>
    {#if item.entries.length}<ul>{#each item.entries as entry (entry)}<li><code>{entry}</code></li>{/each}</ul>{:else}<p><em>None configured.</em></p>{/if}
    <p>Source: {item.source}</p>
  </section>
{/snippet}
<section class="general-settings" aria-label="General settings" aria-busy={loading}>
  <h2>General</h2>
  <p>Read-only view of the effective host configuration. Nothing here can be edited or saved.</p>
  <button type="button" disabled={loading} onclick={()=>void load()}>{model?'Reload':'Load'} host settings</button>
  {#if loading}<p role="status">Loading host settings…</p>{/if}
  {#if model?.kind==='error'}
    <p role="alert">Could not load host settings: {model.message}</p>
    <p>No settings are shown because none were verified. Use the reload button to retry.</p>
  {:else if model?.kind==='ready'}
    {@const view=model.view}
    <section aria-label="Authentication"><h3>Authentication</h3><p>{view.authentication}</p></section>
    <section aria-label="Bind address">
      <h3>Bind address</h3>
      <dl>
        <dt>Configured</dt><dd><code>{view.bind.configured}</code></dd>
        <dt>Bound</dt><dd><code>{view.bind.bound}</code></dd>
        <dt>Source</dt><dd>{view.bind.source}</dd>
      </dl>
      <p>{view.bind.note}</p>
    </section>
    {@render list('Allowed hosts',view.allowedHosts)}
    {@render list('Trusted networks',view.trustedNetworks)}
    <section aria-label="How these settings are managed"><h3>Management</h3><ul>{#each view.runtime as line (line)}<li>{line}</li>{/each}</ul></section>
    <section aria-label="Limits"><h3>Limits</h3><ul>{#each view.limits as line (line)}<li>{line}</li>{/each}</ul></section>
    <section aria-label="Not available in this build"><h3>Not available in this build</h3><ul>{#each view.unavailable as line (line)}<li>{line}</li>{/each}</ul></section>
  {:else if !loading}
    <p role="status">Host settings have not been loaded.</p>
  {/if}
</section>
