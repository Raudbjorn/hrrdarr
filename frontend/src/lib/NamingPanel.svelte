<script lang="ts">
  import { onMount } from 'svelte';
  import type { ColonReplacement, MediaDomain, MovieNamingConfig, MovieNamingUpdate, TvNamingConfig, TvNamingUpdate } from './api.generated';
  import { getTvNaming, updateTvNaming, getTvNamingExamples, getMovieNaming, updateMovieNaming, getMovieNamingExamples } from './api';
  let domain: MediaDomain = $state('tv');
  let config: TvNamingConfig | MovieNamingConfig | null = $state(null);
  let revision = $state(0);
  let renameEnabled = $state(false), replaceIllegal = $state(true), colonReplacement: ColonReplacement = $state('smart'), customColon = $state('');
  let standardEpisode = $state(''), dailyEpisode = $state(''), animeEpisode = $state(''), seriesFolder = $state(''), seasonFolder = $state(''), specialsFolder = $state(''), multiEpisodeStyle = $state('');
  let standardMovie = $state(''), movieFolder = $state('');
  let examples: Record<string, string | null> | null = $state(null);
  let previewLoading = $state(false), previewError = $state(''), previewSkipped = $state(false);
  let loading = $state(false), busy = $state(false), dirty = $state(false), conflict = $state(false), uncertain = $state(false);
  let error = $state(''), notice = $state('');
  let alive = true, loadVersion = 0, previewVersion = 0, previewTimer: ReturnType<typeof setTimeout> | undefined;

  function normalizeFormat(value: string): string | null { return value.trim() === '' ? null : value; }
  function normalizeCustom(value: string): string | null { return value === '' ? null : value; }

  function fillFromTv(item: TvNamingConfig) {
    revision = item.revision; renameEnabled = item.rename_enabled; replaceIllegal = item.replace_illegal_characters; colonReplacement = item.colon_replacement; customColon = item.custom_colon_replacement ?? '';
    standardEpisode = item.standard_episode_format ?? ''; dailyEpisode = item.daily_episode_format ?? ''; animeEpisode = item.anime_episode_format ?? '';
    seriesFolder = item.series_folder_format ?? ''; seasonFolder = item.season_folder_format ?? ''; specialsFolder = item.specials_folder_format ?? '';
    multiEpisodeStyle = item.multi_episode_style === null ? '' : String(item.multi_episode_style);
  }
  function fillFromMovie(item: MovieNamingConfig) {
    revision = item.revision; renameEnabled = item.rename_enabled; replaceIllegal = item.replace_illegal_characters; colonReplacement = item.colon_replacement; customColon = item.custom_colon_replacement ?? '';
    standardMovie = item.standard_movie_format ?? ''; movieFolder = item.movie_folder_format ?? '';
  }

  function buildTvUpdate(): TvNamingUpdate {
    return {
      revision, rename_enabled: renameEnabled, replace_illegal_characters: replaceIllegal, colon_replacement: colonReplacement,
      custom_colon_replacement: colonReplacement === 'custom' ? normalizeCustom(customColon) : null,
      standard_episode_format: normalizeFormat(standardEpisode), daily_episode_format: normalizeFormat(dailyEpisode), anime_episode_format: normalizeFormat(animeEpisode),
      series_folder_format: normalizeFormat(seriesFolder), season_folder_format: normalizeFormat(seasonFolder), specials_folder_format: normalizeFormat(specialsFolder),
      multi_episode_style: multiEpisodeStyle === '' ? null : Number(multiEpisodeStyle),
    };
  }
  function buildMovieUpdate(): MovieNamingUpdate {
    return {
      revision, rename_enabled: renameEnabled, replace_illegal_characters: replaceIllegal, colon_replacement: colonReplacement,
      custom_colon_replacement: colonReplacement === 'custom' ? normalizeCustom(customColon) : null,
      standard_movie_format: normalizeFormat(standardMovie), movie_folder_format: normalizeFormat(movieFolder),
    };
  }

  async function load() {
    const version = ++loadVersion, scope = domain;
    loading = true; error = ''; notice = ''; conflict = false; uncertain = false; config = null; examples = null; previewError = ''; previewSkipped = false; previewLoading = false;
    clearTimeout(previewTimer); ++previewVersion;
    const result = scope === 'tv' ? await getTvNaming() : await getMovieNaming();
    if (!alive || version !== loadVersion) return;
    loading = false;
    if (result.ok) {
      config = result.data;
      if (scope === 'tv') fillFromTv(result.data as TvNamingConfig); else fillFromMovie(result.data as MovieNamingConfig);
      dirty = false; schedulePreview();
    } else error = result.error;
  }
  function changeDomain(next: MediaDomain) {
    if (busy || loading || domain === next) return;
    domain = next; void load();
  }

  function schedulePreview() { clearTimeout(previewTimer); previewTimer = setTimeout(() => void loadPreview(), 400); }
  async function loadPreview() {
    const version = ++previewVersion, scope = domain;
    if (colonReplacement === 'custom' && normalizeCustom(customColon) === null) {
      previewSkipped = true; previewError = ''; previewLoading = false;
      return;
    }
    previewSkipped = false; previewLoading = true; previewError = '';
    const common = { rename_enabled: renameEnabled, replace_illegal_characters: replaceIllegal, colon_replacement: colonReplacement, ...(colonReplacement === 'custom' ? { custom_colon_replacement: customColon } : {}) };
    let result, blanked: string[];
    if (scope === 'tv') {
      const fields = {
        standard_episode_format: normalizeFormat(standardEpisode), daily_episode_format: normalizeFormat(dailyEpisode), anime_episode_format: normalizeFormat(animeEpisode),
        series_folder_format: normalizeFormat(seriesFolder), season_folder_format: normalizeFormat(seasonFolder), specials_folder_format: normalizeFormat(specialsFolder),
      };
      blanked = Object.entries(fields).filter(([, value]) => value === null).map(([key]) => key);
      result = await getTvNamingExamples({
        ...common,
        ...(fields.standard_episode_format !== null ? { standard_episode_format: fields.standard_episode_format } : {}),
        ...(fields.daily_episode_format !== null ? { daily_episode_format: fields.daily_episode_format } : {}),
        ...(fields.anime_episode_format !== null ? { anime_episode_format: fields.anime_episode_format } : {}),
        ...(fields.series_folder_format !== null ? { series_folder_format: fields.series_folder_format } : {}),
        ...(fields.season_folder_format !== null ? { season_folder_format: fields.season_folder_format } : {}),
        ...(fields.specials_folder_format !== null ? { specials_folder_format: fields.specials_folder_format } : {}),
      });
    } else {
      const fields = { standard_movie_format: normalizeFormat(standardMovie), movie_folder_format: normalizeFormat(movieFolder) };
      blanked = Object.entries(fields).filter(([, value]) => value === null).map(([key]) => key);
      result = await getMovieNamingExamples({
        ...common,
        ...(fields.standard_movie_format !== null ? { standard_movie_format: fields.standard_movie_format } : {}),
        ...(fields.movie_folder_format !== null ? { movie_folder_format: fields.movie_folder_format } : {}),
      });
    }
    if (!alive || version !== previewVersion || scope !== domain) return;
    previewLoading = false;
    if (result.ok) {
      const data: Record<string, string | null> = { ...result.data };
      for (const key of blanked) data[key] = null;
      examples = data;
    } else previewError = result.error;
  }

  async function save() {
    if (busy || conflict || uncertain) return;
    error = ''; notice = '';
    const version = loadVersion, scope = domain; busy = true;
    const result = scope === 'tv' ? await updateTvNaming(buildTvUpdate()) : await updateMovieNaming(buildMovieUpdate());
    if (!alive || version !== loadVersion || scope !== domain) return;
    busy = false;
    if (result.ok) {
      config = result.data;
      if (scope === 'tv') fillFromTv(result.data as TvNamingConfig); else fillFromMovie(result.data as MovieNamingConfig);
      dirty = false; notice = 'Naming settings saved.'; schedulePreview();
    } else {
      error = result.error; conflict = result.status === 409; uncertain = !result.status;
      if (uncertain) error += ' The response was lost or unreadable. Reload the saved naming settings before saving again; the change may have committed.';
    }
  }

  function previewLabel(key: string): string {
    if (!examples) return '';
    const value = examples[key];
    return value === null ? 'Not configured.' : `Example: ${value}`;
  }
  function previewStatus(): string {
    if (previewSkipped) return 'Enter a custom colon replacement value above to preview rendered examples.';
    if (previewLoading) return 'Updating preview…';
    if (previewError) return `Preview error: ${previewError}`;
    return '';
  }

  onMount(() => { alive = true; void load(); return () => { alive = false; ++loadVersion; ++previewVersion; clearTimeout(previewTimer); }; });
</script>

<section aria-labelledby="naming-heading" class="naming-panel">
  <div class="heading">
    <div><h1 id="naming-heading">Naming</h1><p>Configure destination filename and folder templates for TV and movies. Filename formats are used by the automated import path when renaming is enabled; folder formats are validated and previewed here but not yet applied automatically.</p></div>
    <nav aria-label="Naming domain"><button aria-pressed={domain === 'tv'} disabled={busy || loading} onclick={() => changeDomain('tv')}>TV</button><button aria-pressed={domain === 'movies'} disabled={busy || loading} onclick={() => changeDomain('movies')}>Movies</button></nav>
  </div>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if notice}<p role="status">{notice}</p>{/if}
  {#if loading}<p role="status">Loading naming settings…</p>{/if}
  {#if conflict || uncertain}
    <div class="conflict">
      <p>{conflict ? 'Naming settings changed elsewhere since these were loaded. Reload before saving again.' : 'The previous save outcome is unknown; it may have committed.'}</p>
      <button disabled={busy} onclick={load}>Reload saved naming settings</button>
    </div>
  {/if}
  {#if config}
    <form onsubmit={(event) => { event.preventDefault(); void save(); }} oninput={() => { dirty = true; schedulePreview(); }} onchange={() => { dirty = true; schedulePreview(); }}>
      <fieldset disabled={busy || conflict || uncertain}><legend>Behavior</legend>
        <label class="check"><input type="checkbox" bind:checked={renameEnabled} />Rename on automated import</label>
        <p>This only gates the automated import path; the preview below always renders regardless of this setting.</p>
        <label class="check"><input type="checkbox" bind:checked={replaceIllegal} />Replace illegal filename characters</label>
        <label>Colon replacement<select bind:value={colonReplacement}>
          <option value="delete">Delete</option><option value="dash">Dash ( - )</option><option value="space_dash">Space + dash ( -)</option><option value="space_dash_space">Space + dash + space ( - )</option><option value="smart">Smart</option><option value="custom">Custom</option>
        </select></label>
        {#if colonReplacement === 'custom'}<label>Custom colon replacement text<input bind:value={customColon} maxlength="16" required placeholder="e.g. ~" /></label>{/if}
      </fieldset>

      <p class="preview-status" role="status" aria-live="polite">{previewStatus()}</p>

      {#if domain === 'tv'}
        <fieldset disabled={busy || conflict || uncertain}><legend>TV filename formats</legend>
          <label>Standard episode format<input bind:value={standardEpisode} maxlength="1024" aria-describedby="preview-standard_episode_format" placeholder="{'{Series Title} - S{season:00}E{episode:00} - {Episode Title} [{Quality Title}]'}" /></label>
          <p class="preview" id="preview-standard_episode_format">{previewLabel('standard_episode_format')}</p>
          <label>Daily episode format<input bind:value={dailyEpisode} maxlength="1024" aria-describedby="preview-daily_episode_format" placeholder="{'{Series Title} - {Air-Date} - {Episode Title} [{Quality Title}]'}" /></label>
          <p class="preview" id="preview-daily_episode_format">{previewLabel('daily_episode_format')}</p>
          <p class="muted">Daily-typed series cannot yet complete an automated download; this format is saved and previewed but currently unreachable from the automated pipeline.</p>
          <label>Anime episode format<input bind:value={animeEpisode} maxlength="1024" aria-describedby="preview-anime_episode_format" placeholder="{'{Series Title} - S{season:00}E{episode:00} - {Episode Title} [{Quality Title}]'}" /></label>
          <p class="preview" id="preview-anime_episode_format">{previewLabel('anime_episode_format')}</p>
          <p class="muted">Anime-typed series cannot yet complete an automated download; this format is saved and previewed but currently unreachable from the automated pipeline.</p>
          <label>Multi-episode style<select bind:value={multiEpisodeStyle}><option value="">Not set</option><option value="0">0</option><option value="1">1</option><option value="2">2</option><option value="3">3</option><option value="4">4</option><option value="5">5</option></select></label>
          <p class="muted">Reserved for future multi-episode rendering; no multi-episode template is applied yet.</p>
        </fieldset>
        <fieldset disabled={busy || conflict || uncertain}><legend>TV folder formats</legend>
          <p class="muted">Folder templates are validated and previewed here but not yet used to create directories during automated import; imports still land in the existing library root.</p>
          <label>Series folder format<input bind:value={seriesFolder} maxlength="1024" aria-describedby="preview-series_folder_format" placeholder="{'{Series Title}'}" /></label>
          <p class="preview" id="preview-series_folder_format">{previewLabel('series_folder_format')}</p>
          <label>Season folder format<input bind:value={seasonFolder} maxlength="1024" aria-describedby="preview-season_folder_format" placeholder="{'Season {season:00}'}" /></label>
          <p class="preview" id="preview-season_folder_format">{previewLabel('season_folder_format')}</p>
          <label>Specials folder format<input bind:value={specialsFolder} maxlength="1024" aria-describedby="preview-specials_folder_format" placeholder="Specials" /></label>
          <p class="preview" id="preview-specials_folder_format">{previewLabel('specials_folder_format')}</p>
        </fieldset>
      {:else}
        <fieldset disabled={busy || conflict || uncertain}><legend>Movie filename format</legend>
          <label>Standard movie format<input bind:value={standardMovie} maxlength="1024" aria-describedby="preview-standard_movie_format" placeholder="{'{Movie Title} ({Release Year}) {Edition Tags} [{Quality Title}]'}" /></label>
          <p class="preview" id="preview-standard_movie_format">{previewLabel('standard_movie_format')}</p>
        </fieldset>
        <fieldset disabled={busy || conflict || uncertain}><legend>Movie folder format</legend>
          <p class="muted">Folder templates are validated and previewed here but not yet used to create directories during automated import; imports still land in the existing library root.</p>
          <label>Movie folder format<input bind:value={movieFolder} maxlength="1024" aria-describedby="preview-movie_folder_format" placeholder="{'{Movie Title} ({Release Year})'}" /></label>
          <p class="preview" id="preview-movie_folder_format">{previewLabel('movie_folder_format')}</p>
        </fieldset>
      {/if}
      <button class="primary" disabled={busy || conflict || uncertain || !dirty}>Save naming settings</button>
    </form>
  {:else if !loading && !error}
    <p>No naming settings loaded.</p>
  {/if}

  <details class="token-reference">
    <summary>Naming token reference</summary>
    <h2>TV tokens</h2>
    <p>{'{Series Title}'}, {'{season:00}'}, {'{episode:00}'}, {'{Episode Title}'}, {'{Air-Date}'}, {'{Quality Title}'}. {'{season:0}'}–{'{season:0000}'} zero-pad to 1–4 digits.</p>
    <h2>Movie tokens</h2>
    <p>{'{Movie Title}'}, {'{Release Year}'}, {'{Edition Tags}'}, {'{Quality Title}'}. {'{Quality Title}'} is the only token shared by both domains.</p>
    <h2>Allowed tokens per field</h2>
    <table>
      <thead><tr><th scope="col">Field</th><th scope="col">Allowed tokens</th></tr></thead>
      <tbody>
        <tr><td>Standard / daily / anime episode format</td><td>{'{Series Title}'}, {'{season:00}'}, {'{episode:00}'}, {'{Episode Title}'}, {'{Air-Date}'}, {'{Quality Title}'}</td></tr>
        <tr><td>Series folder format</td><td>{'{Series Title}'}</td></tr>
        <tr><td>Season / specials folder format</td><td>{'{Series Title}'}, {'{season:00}'}</td></tr>
        <tr><td>Standard movie format</td><td>{'{Movie Title}'}, {'{Release Year}'}, {'{Edition Tags}'}, {'{Quality Title}'}</td></tr>
        <tr><td>Movie folder format</td><td>{'{Movie Title}'}, {'{Release Year}'}, {'{Edition Tags}'}</td></tr>
      </tbody>
    </table>
    <p class="muted">An unrecognized token, a token from the other domain or a token not allowed in a given field is rejected with the offending text named in the error.</p>
  </details>
</section>
<style>
  .heading{display:flex;justify-content:space-between;align-items:start;gap:1rem;flex-wrap:wrap}
  .conflict{border-left:3px solid #a12637;padding:.8rem;background:#fff0f2;margin:1rem 0}
  .conflict button{margin-top:.5rem}
  .preview{margin:0 0 .8rem;font-size:.85rem;color:#245ba8;min-height:1.2em}
  .preview-status{font-size:.85rem;color:#596b7b;min-height:1.2em}
  .muted{font-size:.85rem;color:#596b7b}
  .token-reference{margin-top:2rem;background:#fff;border:1px solid #cbd5de;padding:1rem}
  .token-reference summary{cursor:pointer;font-weight:600}
  .token-reference h2{font-size:.95rem;margin:1rem 0 .3rem}
  .token-reference table{border-collapse:collapse;width:100%;margin-top:.5rem}
  .token-reference th,.token-reference td{text-align:left;padding:.4rem .6rem;border-bottom:1px solid #dce3e9;font-size:.85rem;vertical-align:top}
</style>
