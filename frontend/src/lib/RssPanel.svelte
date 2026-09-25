<script lang="ts">
  import { onMount } from 'svelte';
  import type { ApiPage, Provider, MediaDomain, RssTarget, RssCommand, RssCandidate, RssSchedule } from './api.generated';
  import { getProvider, listProviders, listRssCommands, createRssCommand, cancelRssCommand, listRssCandidates, listRssSchedules, saveRssSchedule } from './api';
  let domain:MediaDomain=$state('tv'), providers:ApiPage<Provider>|null=$state(null), indexer:Provider|null=$state(null), client:Provider|null=$state(null);
  let indexerId=$state(''),clientId=$state(''),interval=$state(900),enabled=$state(false),busy=$state(false),loadingProviders=$state(false),ready=$state(false),uncertain=$state(false),error=$state(''),notice=$state('');
  let commands:ApiPage<RssCommand>|null=$state(null),candidates:ApiPage<RssCandidate>|null=$state(null),schedules:RssSchedule[]=$state([]),commandOffset=$state(0),candidateOffset=$state(0);
  let alive=true,version=0,timer:ReturnType<typeof setTimeout>|undefined;
  const target:RssTarget|null=$derived.by(()=>indexer&&client?{media_type:domain,indexer_id:indexer.id,indexer_revision:indexer.revision,client_id:client.id,client_revision:client.revision}:null);
  const eligible=(p:Provider,kind:'indexer'|'client')=>p.enabled&&p.settings[domain]!==null&&(kind==='client'?p.settings.implementation==='qbittorrent':p.settings.implementation!=='qbittorrent');
  const active=(c:RssCommand)=>['queued','running','retry_wait'].includes(c.status);
  function later(){clearTimeout(timer);if(alive&&!document.hidden)timer=setTimeout(()=>void read(),5000);}
  async function loadProviders(offset=0){
    const v=version;loadingProviders=true;
    const result=await listProviders(offset);if(!alive||v!==version)return;
    if(result.ok)providers=result.data;else error=result.error;
    if(indexer){const fresh=await getProvider(indexer.id);if(!alive||v!==version)return;indexer=fresh.ok&&eligible(fresh.data,'indexer')?fresh.data:null;indexerId=indexer?.id??'';}
    if(client){const fresh=await getProvider(client.id);if(!alive||v!==version)return;client=fresh.ok&&eligible(fresh.data,'client')?fresh.data:null;clientId=client?.id??'';}
    loadingProviders=false;
  }
  async function read(explicit=false){
    if(busy||!alive||document.hidden)return;
    const v=version;busy=true;clearTimeout(timer);
    const [a,b,c]=await Promise.all([listRssCommands(domain,commandOffset),listRssCandidates(domain,candidateOffset),listRssSchedules()]);
    if(!alive||v!==version)return;
    ready=a.ok&&b.ok&&c.ok;
    if(a.ok)commands=a.data;else error=a.error;
    if(b.ok)candidates=b.data;else error=b.error;
    if(c.ok)schedules=c.data;else error=c.error;
    if(ready&&explicit){uncertain=false;error='';notice='RSS status checked. Inspect recorded commands and receipts before another action.';}
    busy=false;later();
  }
  function scope(){++version;clearTimeout(timer);busy=false;ready=false;uncertain=false;indexer=null;client=null;indexerId='';clientId='';commands=null;candidates=null;commandOffset=0;candidateOffset=0;error='';notice='';void loadProviders();void read();}
  async function mutate(action:'run'|'schedule'|'cancel',id?:string){
    if(busy||loadingProviders||!ready||uncertain||(!target&&action!=='cancel'))return;
    busy=true;clearTimeout(timer);error='';notice='';
    const v=version;
    const result=action==='run'?await createRssCommand({target:target!,priority:'normal'}):action==='schedule'?await saveRssSchedule({target:target!,interval_seconds:interval,enabled,revision:schedules.find(s=>s.target.media_type===domain&&s.target.indexer_id===target!.indexer_id&&s.target.client_id===target!.client_id)?.revision}):await cancelRssCommand(id!);
    if(!alive||v!==version)return;
    if(result.ok)notice=action==='run'?'RSS run requested.':action==='schedule'?'RSS schedule saved.':'Undispatched work cancelled. Outstanding submissions still reconcile.';
    else{error=result.error;uncertain=!result.status||result.status>=500;if(uncertain){commandOffset=0;error+=' The request may have committed. Check RSS status before another action. It will not be retried automatically.';}}
    busy=false;await read();
  }
  onMount(()=>{alive=true;void loadProviders();void read();const visibility=()=>{clearTimeout(timer);if(!document.hidden)void read();};document.addEventListener('visibilitychange',visibility);return()=>{alive=false;++version;clearTimeout(timer);document.removeEventListener('visibilitychange',visibility);};});
</script>
<section class="rss-panel" aria-label="RSS automation">
  <h1>RSS automation</h1><p>RSS can submit matching releases to the selected download client. Configure a quality profile and background release delays in Library first.</p>
  <label>RSS media<select bind:value={domain} disabled={busy} onchange={scope}><option value="tv">TV</option><option value="movies">Movies</option></select></label>
  <form onsubmit={e=>{e.preventDefault();void mutate('run');}}>
    <label>RSS indexer<select bind:value={indexerId} disabled={busy||loadingProviders} onchange={()=>indexer=providers?.items.find(p=>p.id===indexerId)??null}><option value="">Choose an indexer</option>{#if indexer&&!providers?.items.some(p=>p.id===indexer!.id)}<option value={indexer.id}>{indexer.name}</option>{/if}{#each providers?.items.filter(p=>eligible(p,'indexer'))??[] as p(p.id)}<option value={p.id}>{p.name} · revision {p.revision}</option>{/each}</select></label>
    <label>RSS download client<select bind:value={clientId} disabled={busy||loadingProviders} onchange={()=>client=providers?.items.find(p=>p.id===clientId)??null}><option value="">Choose a download client</option>{#if client&&!providers?.items.some(p=>p.id===client!.id)}<option value={client.id}>{client.name}</option>{/if}{#each providers?.items.filter(p=>eligible(p,'client'))??[] as p(p.id)}<option value={p.id}>{p.name} · revision {p.revision}</option>{/each}</select></label>
    {#if providers}<div class="actions"><button type="button" disabled={busy||loadingProviders||providers.offset===0} onclick={()=>loadProviders(Math.max(0,providers!.offset-25))}>Previous RSS providers</button><button type="button" disabled={busy||loadingProviders||providers.offset+providers.limit>=providers.total} onclick={()=>loadProviders(providers!.offset+providers!.limit)}>Next RSS providers</button><button type="button" disabled={busy||loadingProviders} onclick={()=>loadProviders(providers!.offset)}>Reload RSS providers</button></div>{/if}
    <button disabled={busy||loadingProviders||!ready||uncertain||!target}>Run RSS now</button>
  </form>
  <form onsubmit={e=>{e.preventDefault();void mutate('schedule');}}><label>RSS interval (seconds)<input type="number" min="60" max="86400" step="1" required bind:value={interval}/></label><label>RSS scheduling<select bind:value={enabled}><option value={false}>Disabled</option><option value={true}>Enabled</option></select></label><button disabled={busy||loadingProviders||!ready||uncertain||!target}>Save RSS schedule</button></form>
  <button disabled={busy} onclick={()=>read(true)}>Check RSS status</button>
  {#if busy}<p role="status">Checking RSS…</p>{/if}{#if error}<p role="alert" class="error">{error}</p>{/if}{#if notice}<p role="status">{notice}</p>{/if}
  <h2>Schedules</h2><ul>{#each schedules.filter(s=>s.target.media_type===domain) as s(s.id)}<li>{s.enabled?'Enabled':'Disabled'} · every {s.interval_seconds}s · indexer {s.target.indexer_id} → client {s.target.client_id} · {s.error_code??'No recorded error'}</li>{:else}<li>No schedules for this media type.</li>{/each}</ul>
  <h2>RSS commands</h2><p>Succeeded means the feed and decision pass ended. Check release receipts for later delays, failures and submission outcomes.</p>{#if commands}<p>{commands.total} commands for {domain}.</p><ul>{#each commands.items as c(c.id)}<li><strong>{c.status}</strong> · {c.id}<p>Fetched {c.fetched}, evaluated {c.evaluated}, rejected {c.rejected}, pending {c.pending}, observed {c.observed}, uncertain {c.uncertain}. {c.error_code??''}</p>{#if active(c)}<button disabled={busy||uncertain||!ready} onclick={()=>mutate('cancel',c.id)}>Cancel RSS {c.id}</button>{/if}</li>{/each}</ul><div class="actions"><button disabled={busy||commandOffset===0} onclick={()=>{commandOffset=Math.max(0,commandOffset-25);void read();}}>Previous RSS commands</button><button disabled={busy||commandOffset+25>=commands.total} onclick={()=>{commandOffset+=25;void read();}}>Next RSS commands</button></div>{/if}
  <h2>Release receipts</h2><p>Needs attention means submission could not be confirmed. It will not be submitted again automatically. Observed means present in the client; client options, download completion and import are not confirmed.</p>
  {#if candidates}<p>{candidates.total} receipts for {domain}.</p><ul>{#each candidates.items as c(c.id)}<li><strong>{c.title}</strong> · {c.status}<p>{c.reasons.join(', ')} {c.error_code??''}</p><p>{c.target?c.target.media_type==='tv'?`TV series ${c.target.series_id}, episodes ${c.target.episode_ids.join(', ')}`:`Movie ${c.target.movie_id}`:'No matched target'} · {c.id}</p>{#if c.not_before}<p>Next check {new Date(c.not_before*1000).toLocaleString()}</p>{/if}</li>{/each}</ul><div class="actions"><button disabled={busy||candidateOffset===0} onclick={()=>{candidateOffset=Math.max(0,candidateOffset-25);void read();}}>Previous RSS receipts</button><button disabled={busy||candidateOffset+25>=candidates.total} onclick={()=>{candidateOffset+=25;void read();}}>Next RSS receipts</button></div>{/if}
</section>
<style>.rss-panel{background:white;border:1px solid #cbd5de;padding:1.5rem}li{padding:.6rem 0;overflow-wrap:anywhere}ul{padding-left:1.25rem}.actions{margin:.75rem 0}</style>
