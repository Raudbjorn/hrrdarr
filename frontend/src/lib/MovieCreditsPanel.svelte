<script lang="ts">
  import { onMount } from 'svelte';
  import type { MovieCredit } from './api.generated';
  import { listMovieCredits } from './api';

  let { movieId }: { movieId: number } = $props();
  let credits: MovieCredit[] = $state([]), failedImages: number[] = $state([]);
  let loading = $state(true), error = $state('');
  let alive = true, generation = 0;
  const groups = [{kind: 'cast', label: 'Cast'}, {kind: 'crew', label: 'Crew'}] as const;
  const personLink = (id: number) => Number.isSafeInteger(id) && id > 0 ? `https://www.themoviedb.org/person/${id}` : null;
  function headshot(credit: MovieCredit): string | null {
    const value = credit.images.find(image => image.cover_type === 'headshot')?.url;
    if (!value || !URL.canParse(value)) return null;
    const url = new URL(value);
    return url.protocol === 'https:' && !url.username && !url.password ? url.href : null;
  }
  async function load() {
    const captured = movieId, version = ++generation;
    loading = true; error = ''; credits = []; failedImages = [];
    const result = await listMovieCredits(captured);
    if (!alive || version !== generation || captured !== movieId) return;
    loading = false;
    if (!result.ok) { error = result.error; return; }
    // The metadata contract caps a movie at 500 credits; never silently show an incomplete cast.
    if (result.data.offset !== 0 || result.data.items.length !== result.data.total || result.data.total > 500) {
      error = 'The complete movie credits could not be loaded.'; return;
    }
    credits = result.data.items;
  }
  onMount(() => { void load(); return () => { alive = false; ++generation; }; });
</script>

<section class="movie-credits" aria-label="Movie credits" aria-busy={loading}>
  <h2>Cast and crew</h2>
  {#if loading}
    <p role="status">Loading movie credits…</p>
  {:else if error}
    <p role="alert" class="error">{error}</p>
    <button type="button" onclick={() => void load()}>Retry movie credits</button>
  {:else}
    <button type="button" onclick={() => void load()}>Reload movie credits</button>
    {#if credits.length === 0}
      <p>No credits available for this movie.</p>
    {:else}
      {#each groups as group (group.kind)}
        {@const members = credits.filter(credit => credit.type === group.kind)}
        <section aria-label={group.label}>
          <h3>{group.label}</h3>
          {#if members.length === 0}<p>No {group.kind} credits available.</p>
          {:else}
            <ul>
              {#each members as credit (credit.id)}
                {@const photo = headshot(credit)}
                {@const link = personLink(credit.person_tmdb_id)}
                <li data-credit-id={credit.id}>
                  {#if photo && !failedImages.includes(credit.id)}
                    <img src={photo} alt="" loading="lazy" referrerpolicy="no-referrer" onerror={() => failedImages = [...failedImages, credit.id]} />
                  {:else}<span class="headshot-fallback">No headshot</span>{/if}
                  <div>
                    {#if link}<a href={link} target="_blank" rel="noopener noreferrer">{credit.person_name}<span class="link-note">{' (TMDb)'}</span></a>
                    {:else}<span>{credit.person_name}</span>{/if}
                    {#if credit.type === 'cast'}<p>{credit.character || 'Character not recorded'}</p>
                    {:else}<p>{credit.job || 'Job not recorded'}</p>{#if credit.department}<p class="department">{credit.department}</p>{/if}{/if}
                  </div>
                </li>
              {/each}
            </ul>
          {/if}
        </section>
      {/each}
    {/if}
  {/if}
</section>

<style>
  .movie-credits { border-top: 1px solid #cbd5de; margin-top: 1.5rem; padding-top: 1rem; }
  h2 { margin-top: 0; }
  ul { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, 15rem), 1fr)); gap: 1rem; list-style: none; padding: 0; }
  li { display: flex; gap: .75rem; min-width: 0; align-items: flex-start; }
  li div { min-width: 0; overflow-wrap: anywhere; }
  img, .headshot-fallback { width: 4rem; height: 6rem; flex: 0 0 4rem; border-radius: .25rem; }
  img { object-fit: cover; }
  .headshot-fallback { display: grid; place-items: center; text-align: center; background: #edf1f4; color: #526574; font-size: .8rem; }
  p { margin-block: .35rem; }
  .department, .link-note { color: #526574; font-size: .85rem; }
</style>
