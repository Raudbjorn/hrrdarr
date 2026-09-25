<script lang="ts">
  import { onMount } from 'svelte';
  import type { ManualImportRequest, MediaTarget, Operation, Mode } from './api.generated';
  import { previewManualImport, executeImport, getImport } from './api';
  let { target, label, oncomplete }: { target: MediaTarget | null; label: string; oncomplete: () => void } = $props();
  type Receipt = { request: ManualImportRequest; label: string; operation: Operation; authorized: boolean };
  const key = 'hrrdarr.import.receipt.v1';
  let source = $state(''), destination = $state(''), mode: Mode = $state('copy');
  let receipt: Receipt | null = $state(null), busy = $state(false), error = $state(''), storageError = $state('');
  let active = true, timer: ReturnType<typeof setTimeout> | undefined;
  function persist() {
    try { if (receipt) localStorage.setItem(key, JSON.stringify(receipt)); else localStorage.removeItem(key); storageError = ''; }
    catch { storageError = 'Browser storage is unavailable. Keep this operation ID to check or resume after reload.'; }
  }
  function accept(operation: Operation) {
    if (!receipt || operation.id !== receipt.operation.id || operation.target.media_type !== receipt.request.target.media_type || operation.target.id !== receipt.request.target.id) { error = 'Operation target does not match the saved confirmation.'; return; }
    const wasComplete = receipt.operation.status === 'complete';
    receipt = { ...receipt, operation }; persist();
    if (operation.status === 'complete' && !wasComplete) oncomplete();
  }
  async function check() {
    if (!receipt || busy) return;
    const id = receipt.operation.id; busy = true; error = '';
    const result = await getImport(id);
    if (!active) return;
    busy = false;
    if (result.ok) accept(result.data); else error = result.error;
    schedule();
  }
  function schedule() {
    clearTimeout(timer);
    if (active && !document.hidden && receipt && !['preview', 'complete'].includes(receipt.operation.status) && !receipt.operation.error_code && !error) timer = setTimeout(check, 5000);
  }
  async function preview() {
    if (!target || receipt || busy) return;
    const request: ManualImportRequest = { target: { ...target }, source, destination, mode };
    const name = label; busy = true; error = '';
    const result = await previewManualImport(request);
    if (!active) return;
    busy = false;
    if (result.ok) { receipt = { request, label: name, operation: result.data, authorized: false }; persist(); }
    else error = `${result.error} No transfer was authorized. If the response was lost, the server may retain an unexecuted preview.`;
  }
  async function execute() {
    if (!receipt || busy) return;
    const id = receipt.operation.id;
    receipt = { ...receipt, authorized: true }; persist();
    busy = true; error = ''; clearTimeout(timer);
    const result = await executeImport(id);
    if (!active) return;
    if (receipt?.operation.id !== id) { busy = false; return; }
    if (result.ok) accept(result.data);
    else { error = `${result.error} Execution may still be running. Check status before resuming; do not create another import.`; }
    if (!result.ok) {
      const status = await getImport(id);
      if (!active) return;
      if (status.ok) { accept(status.data); if (status.data.status === 'complete') error = ''; }
    }
    busy = false; schedule();
  }
  async function editPreview() {
    if (!receipt || receipt.authorized || busy) return;
    const id = receipt.operation.id; busy = true; error = "";
    const result = await getImport(id);
    if (!active) return;
    busy = false;
    if (!result.ok) { error = result.error; return; }
    accept(result.data);
    if (receipt?.operation.id === id && receipt.operation.status === "preview" && !receipt.authorized) {
      source = receipt.request.source; destination = receipt.request.destination; mode = receipt.request.mode; receipt = null; persist();
    }
  }
  onMount(() => {
    active = true;
    try {
      const text = localStorage.getItem(key);
      if (text) {
        if (text.length > 16384) throw new Error();
        const saved = JSON.parse(text) as Receipt;
        if (!saved || !saved.request || !saved.operation || typeof saved.label !== 'string' || typeof saved.operation.id !== 'string' || !/^[0-9a-f-]{36}$/i.test(saved.operation.id) || !['preview','staging','staged','published','committed','complete'].includes(saved.operation.status) || typeof saved.operation.message !== 'string' || (saved.operation.error_code !== null && typeof saved.operation.error_code !== 'string') || !['episode','movie'].includes(saved.request.target?.media_type) || !Number.isSafeInteger(saved.request.target.id) || saved.request.target.id <= 0 || typeof saved.request.source !== 'string' || typeof saved.request.destination !== 'string' || !['copy','move','hardlink'].includes(saved.request.mode) || saved.operation.target?.id !== saved.request.target.id || saved.operation.target?.media_type !== saved.request.target.media_type) throw new Error();
        receipt = { ...saved, authorized: saved.authorized !== false }; void check();
      }
    } catch { storageError = 'Saved import receipt could not be read. Do not start a duplicate transfer; recover its operation ID before continuing.'; }
    const visibility = () => { if (document.hidden) clearTimeout(timer); else schedule(); };
    document.addEventListener("visibilitychange", visibility);
    return () => { active = false; clearTimeout(timer); document.removeEventListener("visibilitychange", visibility); };
  });
</script>
<section aria-labelledby="import-heading" class="import-panel">
  <h2 id="import-heading">Manual import</h2>
  <p>Use existing library and destination directories. Imports accept an empty target and a new destination file; replacement is not supported.</p>
  {#if receipt}
    <h3>Confirmed import: {receipt.label}</h3>
    <dl><dt>Target</dt><dd>{receipt.request.target.media_type} {receipt.request.target.id}</dd><dt>Source</dt><dd>{receipt.request.source}</dd><dt>Destination</dt><dd>{receipt.request.destination}</dd><dt>Transfer mode</dt><dd>{receipt.request.mode}</dd><dt>Operation ID</dt><dd>{receipt.operation.id}</dd><dt>Status</dt><dd aria-live="polite">{receipt.operation.status}{receipt.operation.error_code ? ` (${receipt.operation.error_code})` : ''}</dd></dl>
    <div class="actions">{#if receipt.operation.status === 'preview' && !receipt.authorized}<button disabled={busy} onclick={editPreview}>Edit unexecuted preview</button>{/if}<button disabled={busy} onclick={check}>Check status</button>{#if receipt.operation.status !== 'complete'}<button class="primary" disabled={busy} onclick={execute}>{receipt.operation.status === 'preview' && !receipt.authorized ? 'Execute import' : 'Resume import'}</button>{:else}<button disabled={busy} onclick={() => {receipt = null; persist(); source = ''; destination = ''; error = '';}}>Prepare another import</button>{/if}</div>
    {#if receipt.operation.status === 'preview' && !receipt.authorized}<p>No transfer has been authorized from this confirmation. Execute import authorizes the confirmed {receipt.request.mode} operation.</p>{/if}
  {:else}
    <p>{target ? `Selected target: ${label}` : 'Select a movie or an episode without a file.'}</p>
    <form onsubmit={(event) => {event.preventDefault(); void preview();}}>
      <label>Source file<input bind:value={source} required /></label><label>Destination file<input bind:value={destination} required /></label>
      <label>Transfer mode<select bind:value={mode}><option value="copy">Copy (keep source)</option><option value="move">Move (retire source after import)</option><option value="hardlink">Hardlink (same filesystem)</option></select></label>
      <button class="primary" disabled={!target || busy || !!storageError}>Preview import</button>
    </form>
  {/if}
  {#if busy}<p role="status">Checking import…</p>{/if}{#if error}<p role="alert" class="error">{error}</p>{/if}{#if storageError}<p role="alert" class="error">{storageError}</p>{/if}
</section>
