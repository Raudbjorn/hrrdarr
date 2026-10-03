<script lang="ts">
  import { SETTINGS_SECTIONS, UNAVAILABLE_SECTIONS, hashFor } from './routes';
</script>

<section class="settings-index">
  <h1>Settings</h1>
  <nav aria-label="Settings sections">
    <ul>
      {#each SETTINGS_SECTIONS as section (section.view)}
        <li><a href={hashFor(section.view)}>{section.label}</a> <span>{section.description}</span></li>
      {/each}
    </ul>
  </nav>
  <h2>Not available in this build</h2>
  <ul aria-label="Settings sections not available in this build">
    {#each UNAVAILABLE_SECTIONS as name (name)}<li>{name}</li>{/each}
  </ul>
</section>

<style>
  .settings-index ul{padding-left:1.25rem}.settings-index li{margin:.35rem 0}.settings-index span{color:#5b6e7c}
</style>
