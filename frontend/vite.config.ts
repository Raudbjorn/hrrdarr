import { defineConfig, loadEnv } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

export default defineConfig(({ mode }) => {
// Explicit server-side development target; never infer a running local service.
const target = loadEnv(mode, '.', 'HRRDARR_').HRRDARR_API_ORIGIN;
if (target) {
  const url = new URL(target);
  if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || url.pathname !== '/' || url.search || url.hash) {
    throw new Error('HRRDARR_API_ORIGIN must be an HTTP(S) origin without credentials or a path.');
  }
}
return { plugins: [svelte()], server: { host: '127.0.0.1', ...(target ? { proxy: { '/api': { target } } } : {}) } };
});
