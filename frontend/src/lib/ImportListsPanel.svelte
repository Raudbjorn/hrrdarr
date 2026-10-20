<script lang="ts">
  import {onMount} from 'svelte';
  import type {ImportList,ImportListImplementation,ImportListRun,ImportListItem,ImportListExclusion,MediaDomain,QualityProfileSummary,RootFolder,Tag} from './api.generated';
  import {listImportLists,getImportListSchema,createImportList,updateImportList,deleteImportList,testImportListDraft,testSavedImportList,syncImportList,syncAllImportLists,listImportListRuns,listImportListItems,listImportListExclusions,addImportListExclusion,deleteImportListExclusion,bulkDeleteImportListExclusions,listQualityProfiles,listRootFolders,listTags,IMPORT_LIST_ITEM_STATES,IMPORT_LIST_MAX_BULK_DELETE,type SettingsWriteState,type Result} from './api';
  import {blankDraft,draftFromList,buildInput,buildExclusion,describeTest,describeRun,describeError,isRunActive,pollDelay,MAX_POLLS,formatTime,listStatus,monitorOptions,domainFields,supportedDomains,withDomain,pageCount,SERIES_TYPES,AVAILABILITIES,DOMAIN_LABELS,ITEM_STATE_LABELS,type ImportListDraft} from './import-list-draft';

  const PAGE=25,LOOKUP_PAGE=50,LOOKUP_MAX_PAGES=20,RUN_HISTORY=10;
  type Lookups={profiles:Record<MediaDomain,QualityProfileSummary[]>;roots:Record<MediaDomain,RootFolder[]>;tags:Record<MediaDomain,Tag[]>};

  let schema=$state<ImportListImplementation[]>([]),lists=$state<ImportList[]>([]);
  let lookups=$state<Lookups>({profiles:{tv:[],movies:[]},roots:{tv:[],movies:[]},tags:{tv:[],movies:[]}});
  let draft=$state<ImportListDraft|null>(null),editing=$state<ImportList|null>(null);
  let pickImpl=$state(''),pickMedia=$state<MediaDomain>('tv');
  let lookupsReady=$state(false),loading=$state(false),write=$state<SettingsWriteState>('idle'),error=$state(''),notice=$state(''),testing=$state(false),testNote=$state(''),lookupError=$state('');
  let runs=$state<Record<string,ImportListRun>>({}),runErrors=$state<Record<string,string>>({});
  let historyFor=$state<string|null>(null),history=$state<ImportListRun[]>([]),historyError=$state('');
  let itemsFor=$state<ImportList|null>(null),itemState=$state(''),itemPage=$state(0),itemRows=$state<ImportListItem[]>([]),itemTotal=$state(0),itemsLoading=$state(false),itemsError=$state('');
  let exFilter=$state<''|MediaDomain>(''),exPage=$state(0),exRows=$state<ImportListExclusion[]>([]),exTotal=$state(0),exLoading=$state(false),exError=$state(''),exNotice=$state(''),exSelected=$state<number[]>([]),exBusy=$state(false);
  let exAdd=$state({media:'tv' as MediaDomain,id:'',title:'',year:''});
  let alive=true,itemVersion=0,exVersion=0;
  const pollTokens=new Map<string,number>(),paused=new Set<string>();
  const busy=$derived(write==='busy'||testing);
  const impl=$derived(draft?schema.find(s=>s.implementation===draft!.implementation)??null:null);
  const pickable=$derived(schema.find(s=>s.implementation===pickImpl)??null);
  const sleep=(ms:number)=>new Promise<void>(r=>setTimeout(r,ms));
  const implLabel=(name:string)=>schema.find(s=>s.implementation===name)?.label??name;

  async function loadAll<T>(fetchPage:(offset:number)=>Promise<Result<{items:T[];total:number}>>):Promise<Result<T[]>> {
    const out:T[]=[];
    for(let i=0;i<LOOKUP_MAX_PAGES;i++){
      const r=await fetchPage(i*LOOKUP_PAGE);
      if(!r.ok)return r;
      out.push(...r.data.items);
      if(out.length>=r.data.total||r.data.items.length===0)break;
    }
    return {ok:true,data:out};
  }
  async function loadLookups() {
    lookupError='';
    const [pt,pm,rt,rm,tt,tm]=await Promise.all([
      loadAll(o=>listQualityProfiles('tv',o)),loadAll(o=>listQualityProfiles('movies',o)),
      loadAll(o=>listRootFolders('tv',o)),loadAll(o=>listRootFolders('movies',o)),listTags('tv'),listTags('movies')]);
    if(!alive)return;
    lookupsReady=true;
    const bad=[pt,pm,rt,rm,tt,tm].find(r=>!r.ok);
    if(bad&&!bad.ok)lookupError=`Profiles, root folders or tags could not be loaded: ${bad.error}`;
    lookups={
      profiles:{tv:pt.ok?pt.data:[],movies:pm.ok?pm.data:[]},
      roots:{tv:rt.ok?rt.data:[],movies:rm.ok?rm.data:[]},
      tags:{tv:tt.ok?tt.data:[],movies:tm.ok?tm.data:[]},
    };
  }
  async function loadLists():Promise<boolean> {
    const l=await listImportLists();
    if(!alive)return false;
    if(!l.ok){error=l.error;return false;}
    lists=l.data.items;
    return true;
  }
  async function load() {
    loading=true;error='';
    const s=await getImportListSchema();
    if(!alive)return;
    if(!s.ok){loading=false;error=s.error;return;}
    schema=s.data.items;
    if(!pickImpl&&schema[0]){pickImpl=schema[0].implementation;pickMedia=supportedDomains(schema[0])[0]??'tv';}
    const ok=await loadLists();
    loading=false;
    if(!ok)return;
    void loadLookups();
    void loadExclusions();
    // Pick up runs already in flight (scheduled or from another tab) without blocking the list.
    for(const l of lists){
      const r=await listImportListRuns(l.id,1);
      if(!alive)return;
      const latest=r.ok?r.data.items[0]:undefined;
      if(latest){runs[l.id]=latest;if(isRunActive(latest))void poll(l.id);}
    }
  }

  function choosePick(name:string) {
    pickImpl=name;
    const found=schema.find(s=>s.implementation===name);
    const domains=found?supportedDomains(found):[];
    if(found&&!domains.includes(pickMedia))pickMedia=domains[0]??'tv';
  }
  function startAdd() {
    const found=pickable;
    if(!found||busy||!lookupsReady||!supportedDomains(found).includes(pickMedia))return;
    editing=null;draft=blankDraft(found,pickMedia);notice='';error='';testNote='';
  }
  function startEdit(l:ImportList) {
    const found=schema.find(s=>s.implementation===l.implementation);
    if(!found||busy||!lookupsReady)return;
    editing=l;draft=draftFromList(l,found);notice='';error='';testNote='';
  }
  function cancel() {if(busy)return;draft=null;editing=null;error='';testNote='';}
  function built() {
    if(!draft||!impl)return null;
    const input=buildInput(draft,impl,editing?.secrets_configured??[]);
    if(!input.ok){error=input.error;return null;}
    return input.data;
  }
  async function save() {
    if(!draft||busy)return;
    const input=built();
    if(!input)return;
    write='busy';error='';notice='';
    const result=editing?await updateImportList(editing.id,input):await createImportList(input);
    if(!alive)return;
    if(!result.ok){write=result.status===undefined||result.status>=500?'uncertain':'idle';error=result.error;return;}
    write='idle';notice=`Import list "${result.data.name}" saved.`;
    // Typed secrets leave state with the draft.
    draft=null;editing=null;testNote='';await loadLists();
  }
  async function runTest() {
    if(!draft||busy)return;
    const input=built();
    if(!input)return;
    testing=true;error='';testNote='';
    const result=await testImportListDraft({draft:input,...(editing?{existing_id:editing.id}:{})});
    if(!alive)return;
    testing=false;
    if(!result.ok){error=result.error;return;}
    testNote=describeTest(result.data);
  }
  async function testSaved(l:ImportList) {
    if(busy)return;
    testing=true;error='';notice='';
    const result=await testSavedImportList(l.id);
    if(!alive)return;
    testing=false;
    if(!result.ok){error=result.error;return;}
    notice=`${l.name}: ${describeTest(result.data)}`;
  }
  async function remove(l:ImportList) {
    if(busy||!window.confirm(`Delete import list "${l.name}"? Its sync history is discarded; series and movies it added stay in the library.`))return;
    write='busy';error='';notice='';
    const result=await deleteImportList(l.id);
    if(!alive)return;
    write='idle';
    if(!result.ok){error=result.error;return;}
    pollTokens.set(l.id,(pollTokens.get(l.id)??0)+1);
    if(historyFor===l.id)historyFor=null;
    if(itemsFor?.id===l.id)itemsFor=null;
    delete runs[l.id];
    notice=`Import list "${l.name}" deleted.`;await loadLists();
  }

  // Sync runs: sequential polling that stops on a terminal run, when hidden, or on unmount.
  async function poll(id:string) {
    const token=(pollTokens.get(id)??0)+1;
    pollTokens.set(id,token);paused.delete(id);
    for(let attempt=0;attempt<MAX_POLLS;attempt++){
      await sleep(pollDelay(attempt));
      if(!alive||pollTokens.get(id)!==token)return;
      if(document.hidden){paused.add(id);return;}
      const r=await listImportListRuns(id,RUN_HISTORY);
      if(!alive||pollTokens.get(id)!==token)return;
      if(!r.ok){runErrors[id]=`Run status could not be read: ${r.error}`;return;}
      delete runErrors[id];
      const current=runs[id];
      const fresh=r.data.items.find(x=>x.id===current?.id)??(current?undefined:r.data.items[0]);
      if(!fresh)return;
      runs[id]=fresh;
      if(!isRunActive(fresh)){await finishRun(id);return;}
    }
    runErrors[id]='Stopped watching this run. Reload to check it again.';
  }
  async function finishRun(id:string) {
    await loadLists();
    if(!alive)return;
    if(historyFor===id)await loadHistory(id);
    if(itemsFor?.id===id){itemsFor=lists.find(l=>l.id===id)??itemsFor;await loadItems();}
  }
  function onVisibility() {
    if(document.hidden)return;
    for(const id of [...paused])void poll(id);
  }
  async function syncNow(l:ImportList) {
    if(busy)return;
    error='';notice='';delete runErrors[l.id];
    const r=await syncImportList(l.id);
    if(!alive)return;
    if(!r.ok){error=r.error;return;}
    runs[l.id]=r.data;
    if(isRunActive(r.data))void poll(l.id);else await finishRun(l.id);
  }
  async function syncAll() {
    if(busy)return;
    error='';notice='';
    const r=await syncAllImportLists();
    if(!alive)return;
    if(!r.ok){error=r.error;return;}
    for(const run of r.data.runs){runs[run.list_id]=run;if(isRunActive(run))void poll(run.list_id);}
    notice=`Sync requested for ${r.data.runs.length} list${r.data.runs.length===1?'':'s'}; ${r.data.skipped} skipped (disabled or already syncing).`;
  }

  async function loadHistory(id:string) {
    const r=await listImportListRuns(id,RUN_HISTORY);
    if(!alive||historyFor!==id)return;
    historyError=r.ok?'':r.error;
    if(r.ok)history=r.data.items;
  }
  async function toggleHistory(l:ImportList) {
    if(historyFor===l.id){historyFor=null;return;}
    historyFor=l.id;history=[];historyError='';
    await loadHistory(l.id);
  }
  async function loadItems() {
    const list=itemsFor;
    if(!list)return;
    const version=++itemVersion;
    itemsLoading=true;itemsError='';
    const r=await listImportListItems(list.id,{...(itemState?{state:itemState}:{}),limit:PAGE,offset:itemPage*PAGE});
    if(!alive||version!==itemVersion)return;
    itemsLoading=false;
    if(!r.ok){itemsError=r.error;return;}
    itemRows=r.data.items;itemTotal=r.data.total;
  }
  function openItems(l:ImportList) {
    if(itemsFor?.id===l.id){itemsFor=null;return;}
    itemsFor=l;itemState='';itemPage=0;itemRows=[];itemTotal=0;void loadItems();
  }
  function itemIds(i:ImportListItem) {
    return [i.tvdb_id!==null?`TVDB ${i.tvdb_id}`:'',i.tmdb_id!==null?`TMDb ${i.tmdb_id}`:'',i.imdb_id??''].filter(Boolean).join(', ');
  }

  async function loadExclusions() {
    const version=++exVersion;
    exLoading=true;exError='';
    const r=await listImportListExclusions({...(exFilter?{media_type:exFilter}:{}),limit:PAGE,offset:exPage*PAGE});
    if(!alive||version!==exVersion)return;
    exLoading=false;
    if(!r.ok){exError=r.error;return;}
    if(r.data.items.length===0&&r.data.total>0&&exPage>0){exPage=pageCount(r.data.total,PAGE)-1;void loadExclusions();return;}
    exRows=r.data.items;exTotal=r.data.total;
    const visible=new Set(exRows.map(e=>e.id));
    exSelected=exSelected.filter(id=>visible.has(id));
  }
  function setFilter(value:string) {exFilter=value==='tv'||value==='movies'?value:'';exPage=0;exSelected=[];void loadExclusions();}
  function gotoExPage(p:number) {exPage=p;exSelected=[];void loadExclusions();}
  async function addExclusion() {
    if(exBusy)return;
    const input=buildExclusion(exAdd.media,exAdd.id,exAdd.title,exAdd.year);
    if(!input.ok){exError=input.error;return;}
    exBusy=true;exError='';exNotice='';
    const r=await addImportListExclusion(input.data);
    if(!alive)return;
    exBusy=false;
    if(!r.ok){exError=r.error;return;}
    exNotice=`Excluded "${r.data.title}" (${DOMAIN_LABELS[r.data.media_type]} ${r.data.external_id}).`;
    exAdd={...exAdd,id:'',title:'',year:''};
    await loadExclusions();
  }
  async function removeExclusion(e:ImportListExclusion) {
    if(exBusy||!window.confirm(`Remove the exclusion for "${e.title}"? Lists may add it again on their next sync.`))return;
    exBusy=true;exError='';exNotice='';
    const r=await deleteImportListExclusion(e.id);
    if(!alive)return;
    exBusy=false;
    if(!r.ok){exError=r.error;return;}
    exNotice=`Exclusion for "${e.title}" removed.`;
    await loadExclusions();
  }
  async function removeSelected() {
    const ids=[...exSelected];
    if(exBusy||ids.length===0||!window.confirm(`Remove ${ids.length} exclusion${ids.length===1?'':'s'}? Lists may add these titles again on their next sync.`))return;
    exBusy=true;exError='';exNotice='';
    const r=await bulkDeleteImportListExclusions(ids);
    if(!alive)return;
    exBusy=false;
    if(!r.ok){exError=r.error;return;}
    exNotice=`${ids.length} exclusion${ids.length===1?'':'s'} removed.`;exSelected=[];
    await loadExclusions();
  }
  const toggleId=(list:number[],id:number)=>list.includes(id)?list.filter(t=>t!==id):[...list,id];
  const allSelected=$derived(exRows.length>0&&exRows.every(e=>exSelected.includes(e.id)));

  onMount(()=>{
    void load();
    document.addEventListener('visibilitychange',onVisibility);
    return()=>{alive=false;document.removeEventListener('visibilitychange',onVisibility);};
  });
</script>
<section aria-label="Import lists" aria-busy={loading||busy}>
  <h2>Import lists</h2>
  <p>Import lists add series and movies from external sources on a schedule. Credentials are write-only: they are stored encrypted and never shown again. A list with automatic add turned off records what it finds without adding anything.</p>
  {#if loading}<p role="status">Loading import lists…</p>{/if}
  {#if error}<p role="alert">{error}</p>{/if}
  {#if lookupError}<p role="alert">{lookupError}</p>{/if}
  {#if notice}<p role="status" aria-label="Notice">{notice}</p>{/if}
  {#if write==='uncertain'}<p role="alert">Write outcome unknown. Reload before another write.</p>{/if}
  {#if !draft}
    <div class="toolbar">
      <button type="button" disabled={busy||loading||lists.length===0} onclick={syncAll}>Sync all</button>
    </div>
    <ul class="list-items" aria-label="Configured import lists">
      {#each lists as l (l.id)}
        {@const run=runs[l.id]}
        <li>
          <div class="head">
            <strong>{l.name}</strong>
            <span class="badge">{implLabel(l.implementation)}</span>
            <span class="badge" aria-label={`Media type ${DOMAIN_LABELS[l.media_type]}`}>{DOMAIN_LABELS[l.media_type]}</span>
            <span class="badge">{l.enabled?'Enabled':'Disabled'}</span>
            <span class="badge">{l.enable_automatic_add?'Automatic add':'Record only'}</span>
          </div>
          <p class="meta">Every {l.interval_minutes} min{l.tag_ids.length?`, ${l.tag_ids.length} tag${l.tag_ids.length===1?'':'s'}`:''}. {listStatus(l)}.</p>
          {#if run}<p role="status" aria-label={`Sync status ${l.name}`}>{describeRun(run)}</p>{/if}
          {#if runErrors[l.id]}<p role="alert">{runErrors[l.id]}</p>{/if}
          <div class="actions">
            <button type="button" disabled={busy||isRunActive(run)} onclick={()=>syncNow(l)} aria-label={`Sync now ${l.name}`}>Sync now</button>
            <button type="button" disabled={busy||!lookupsReady} onclick={()=>startEdit(l)} aria-label={`Edit ${l.name}`}>Edit</button>
            <button type="button" disabled={busy} onclick={()=>testSaved(l)} aria-label={`Test ${l.name}`}>Test</button>
            <button type="button" aria-expanded={historyFor===l.id} onclick={()=>toggleHistory(l)} aria-label={`Recent syncs ${l.name}`}>Recent syncs</button>
            <button type="button" aria-expanded={itemsFor?.id===l.id} onclick={()=>openItems(l)} aria-label={`Items ${l.name}`}>Items</button>
            <button type="button" disabled={busy} onclick={()=>remove(l)} aria-label={`Delete ${l.name}`}>Delete</button>
          </div>
          {#if historyFor===l.id}
            <div role="group" aria-label={`Recent syncs for ${l.name}`} class="sub">
              {#if historyError}<p role="alert">{historyError}</p>{/if}
              <ul>{#each history as h (h.id)}<li>{formatTime(h.created_at)} ({h.origin}): {describeRun(h)}</li>{:else}<li>No syncs yet.</li>{/each}</ul>
            </div>
          {/if}
          {#if itemsFor?.id===l.id}
            <div role="group" aria-label={`Items for ${l.name}`} class="sub">
              <label>Show
                <select aria-label="Item state" bind:value={itemState} onchange={()=>{itemPage=0;void loadItems();}}>
                  <option value="">All items</option>
                  {#each IMPORT_LIST_ITEM_STATES as s (s)}<option value={s}>{ITEM_STATE_LABELS[s]}</option>{/each}
                </select>
              </label>
              {#if itemsLoading}<p role="status">Loading items…</p>{/if}
              {#if itemsError}<p role="alert">{itemsError}</p>{/if}
              <ul aria-label="List items">
                {#each itemRows as i (i.key)}
                  <li>{i.title}{i.year?` (${i.year})`:''} <span class="badge">{ITEM_STATE_LABELS[i.state]??i.state}</span> <small>{itemIds(i)}</small>{#if i.error_code} <small>{describeError(i.error_code)}</small>{/if}</li>
                {:else}{#if !itemsLoading}<li>No items{itemState?` in this state`:' recorded yet; sync the list to fetch them'}.</li>{/if}{/each}
              </ul>
              <div class="actions">
                <button type="button" disabled={itemPage===0||itemsLoading} onclick={()=>{itemPage--;void loadItems();}}>Previous items</button>
                <span>Page {itemPage+1} of {pageCount(itemTotal,PAGE)} ({itemTotal} item{itemTotal===1?'':'s'})</span>
                <button type="button" disabled={itemPage+1>=pageCount(itemTotal,PAGE)||itemsLoading} onclick={()=>{itemPage++;void loadItems();}}>Next items</button>
              </div>
            </div>
          {/if}
        </li>
      {:else}
        {#if !loading}<li>No import lists are configured.</li>{/if}
      {/each}
    </ul>
    <form class="add" onsubmit={e=>{e.preventDefault();startAdd();}} aria-label="Add import list">
      <label>Implementation
        <select aria-label="Implementation" disabled={busy||loading} value={pickImpl} onchange={e=>choosePick(e.currentTarget.value)}>
          {#each schema as s (s.implementation)}<option value={s.implementation}>{s.label}</option>{/each}
        </select>
      </label>
      <label>Media type
        <select aria-label="List media type" disabled={busy||loading} bind:value={pickMedia}>
          {#each pickable?supportedDomains(pickable):[] as d (d)}<option value={d}>{DOMAIN_LABELS[d]}</option>{/each}
        </select>
      </label>
      <button type="submit" disabled={busy||loading||!lookupsReady||!pickable}>Add list</button>
    </form>
  {:else if impl}
    <form onsubmit={e=>{e.preventDefault();void save();}} aria-label={editing?`Edit ${editing.name}`:`Add ${impl.label}`}>
      <fieldset disabled={busy}>
        <legend>{editing?'Edit':'Add'} {impl.label} ({DOMAIN_LABELS[draft.media]})</legend>
        <p>{impl.description}</p>
        <label>Name<input bind:value={draft.name} autocomplete="off" required/></label>
        <label class="inline"><input type="checkbox" bind:checked={draft.enabled}/> Enabled</label>
        {#if !editing&&supportedDomains(impl).length>1}
          <label>Media type
            <select aria-label="Media type" value={draft.media} onchange={e=>{if(draft)draft=withDomain(draft,e.currentTarget.value==='movies'?'movies':'tv');}}>
              {#each supportedDomains(impl) as d (d)}<option value={d}>{DOMAIN_LABELS[d]}</option>{/each}
            </select>
          </label>
        {/if}
        {#each impl.fields as f (f.name)}
          {#if f.secret}
            <label>{f.label}{f.required?' *':''}
              <input type="password" autocomplete="new-password" placeholder={editing?.secrets_configured.includes(f.name)?'Configured (leave blank to keep)':''} bind:value={draft.secrets[f.name]}/>
            </label>
            {#if editing?.secrets_configured.includes(f.name)&&!f.required}<label class="inline"><input type="checkbox" checked={draft.cleared.includes(f.name)} onchange={()=>draft!.cleared=draft!.cleared.includes(f.name)?draft!.cleared.filter(c=>c!==f.name):[...draft!.cleared,f.name]}/> Clear {f.label}</label>{/if}
          {:else if f.kind==='boolean'}
            <label class="inline"><input type="checkbox" checked={draft.settings[f.name]===true} onchange={e=>draft!.settings[f.name]=e.currentTarget.checked}/> {f.label}</label>
          {:else if f.kind==='select'}
            <label>{f.label}<select aria-label={f.label} bind:value={draft.settings[f.name]}>{#each f.options as o (o)}<option value={o}>{o}</option>{/each}</select></label>
          {:else if f.kind==='number'}
            <label>{f.label}{f.required?' *':''}<input type="number" min={f.min??undefined} max={f.max??undefined} step="1" bind:value={draft.settings[f.name]}/></label>
          {:else}
            <label>{f.label}{f.required?' *':''}<input type={f.kind==='url'?'url':'text'} autocomplete="off" spellcheck="false" bind:value={draft.settings[f.name]}/></label>
          {/if}
          {#if f.help}<small>{f.help}</small>{/if}
        {/each}
        <fieldset class="scope" aria-label="Defaults for added items">
          <legend>Defaults for added {draft.media==='tv'?'series':'movies'}</legend>
          <label>Quality profile
            <select aria-label="Quality profile" bind:value={draft.qualityProfileId}><option value="">Not set</option>{#each lookups.profiles[draft.media] as p (p.id)}<option value={String(p.id)}>{p.name}</option>{/each}</select>
          </label>
          <label>Root folder
            <select aria-label="Root folder" bind:value={draft.rootFolderId}><option value="">Not set</option>{#each lookups.roots[draft.media] as r (r.id)}<option value={String(r.id)}>{r.path}</option>{/each}</select>
          </label>
          <label>Monitor
            <select aria-label="Monitor" bind:value={draft.monitor}>{#each monitorOptions(draft.media) as o (o.value)}<option value={o.value}>{o.label}</option>{/each}</select>
          </label>
          {#if domainFields(draft.media).seriesType}
            <label>Series type
              <select aria-label="Series type" bind:value={draft.seriesType}>{#each SERIES_TYPES as o (o.value)}<option value={o.value}>{o.label}</option>{/each}</select>
            </label>
          {/if}
          {#if domainFields(draft.media).seasonFolder}
            <label class="inline"><input type="checkbox" bind:checked={draft.seasonFolder}/> Use season folders</label>
          {/if}
          {#if domainFields(draft.media).minimumAvailability}
            <label>Minimum availability
              <select aria-label="Minimum availability" bind:value={draft.minimumAvailability}>{#each AVAILABILITIES as o (o.value)}<option value={o.value}>{o.label}</option>{/each}</select>
            </label>
          {/if}
          <label class="inline"><input type="checkbox" bind:checked={draft.enableAutomaticAdd}/> Enable automatic add</label>
          <label class="inline"><input type="checkbox" bind:checked={draft.searchOnAdd}/> Search on add</label>
          <label>Sync interval (minutes, at least {impl.min_interval_minutes})
            <input type="number" min={impl.min_interval_minutes} max="43200" step="1" bind:value={draft.intervalMinutes}/>
          </label>
          <div role="group" aria-label="Tags">
            <small>Tags applied to added {draft.media==='tv'?'series':'movies'}</small>
            {#each lookups.tags[draft.media] as t (t.id)}
              <label class="inline"><input type="checkbox" checked={draft.tagIds.includes(t.id)} onchange={()=>draft!.tagIds=toggleId(draft!.tagIds,t.id)}/> {t.label}</label>
            {:else}<small>No {DOMAIN_LABELS[draft.media]} tags exist.</small>{/each}
          </div>
        </fieldset>
        <div class="actions">
          <button type="submit">Save</button>
          <button type="button" onclick={runTest}>Test</button>
          <button type="button" onclick={cancel}>Cancel</button>
        </div>
        {#if testing}<p role="status">Testing the list…</p>{/if}
        {#if testNote}<p role="status" aria-label="Test result">{testNote}</p>{/if}
      </fieldset>
    </form>
  {/if}

  <section aria-label="Import list exclusions" aria-busy={exLoading||exBusy}>
    <h3>Exclusions</h3>
    <p>No import list will add an excluded title. TV exclusions use TVDB ids and movie exclusions TMDb ids.</p>
    {#if exError}<p role="alert" aria-label="Exclusion error">{exError}</p>{/if}
    {#if exNotice}<p role="status" aria-label="Exclusion notice">{exNotice}</p>{/if}
    <form class="add" onsubmit={e=>{e.preventDefault();void addExclusion();}} aria-label="Add exclusion">
      <label>Media type
        <select aria-label="Exclusion media type" bind:value={exAdd.media}><option value="tv">TV</option><option value="movies">Movies</option></select>
      </label>
      <label>{exAdd.media==='tv'?'TVDB ID':'TMDb ID'}<input inputmode="numeric" autocomplete="off" bind:value={exAdd.id}/></label>
      <label>Title<input autocomplete="off" bind:value={exAdd.title}/></label>
      <label>Year (optional)<input inputmode="numeric" autocomplete="off" bind:value={exAdd.year}/></label>
      <button type="submit" disabled={exBusy}>Add exclusion</button>
    </form>
    <label>Show exclusions for
      <select aria-label="Exclusion media type filter" value={exFilter} onchange={e=>setFilter(e.currentTarget.value)}>
        <option value="">All</option><option value="tv">TV</option><option value="movies">Movies</option>
      </select>
    </label>
    {#if exLoading}<p role="status">Loading exclusions…</p>{/if}
    {#if exRows.length>0}
      <label class="inline"><input type="checkbox" checked={allSelected} onchange={e=>exSelected=e.currentTarget.checked?exRows.map(r=>r.id):[]}/> Select all on this page</label>
      <button type="button" disabled={exBusy||exSelected.length===0||exSelected.length>IMPORT_LIST_MAX_BULK_DELETE} onclick={removeSelected}>Delete selected ({exSelected.length})</button>
    {/if}
    <ul class="list-items" aria-label="Exclusions">
      {#each exRows as e (e.id)}
        <li>
          <label class="inline"><input type="checkbox" checked={exSelected.includes(e.id)} onchange={()=>exSelected=toggleId(exSelected,e.id)} aria-label={`Select ${e.title}`}/> <strong>{e.title}</strong></label>
          <span class="badge">{DOMAIN_LABELS[e.media_type]}</span>
          <span class="meta">{e.media_type==='tv'?'TVDB':'TMDb'} {e.external_id}{e.year?`, ${e.year}`:''}</span>
          <button type="button" disabled={exBusy} onclick={()=>removeExclusion(e)} aria-label={`Remove exclusion ${e.title}`}>Remove</button>
        </li>
      {:else}{#if !exLoading}<li>No exclusions.</li>{/if}{/each}
    </ul>
    <div class="actions">
      <button type="button" disabled={exPage===0||exLoading} onclick={()=>gotoExPage(exPage-1)}>Previous exclusions</button>
      <span>Page {exPage+1} of {pageCount(exTotal,PAGE)} ({exTotal} exclusion{exTotal===1?'':'s'})</span>
      <button type="button" disabled={exPage+1>=pageCount(exTotal,PAGE)||exLoading} onclick={()=>gotoExPage(exPage+1)}>Next exclusions</button>
    </div>
  </section>
</section>
<style>
  section{margin-top:1.5rem;min-width:0}fieldset{display:grid;gap:.6rem;margin-top:1rem;border:1px solid #cbd5de;padding:1rem;min-width:0}label{display:grid;gap:.2rem;min-width:0}label.inline{display:flex;gap:.4rem;align-items:center;flex-wrap:wrap}input[type=text],input[type=url],input[type=password],input[type=number],input:not([type]),select{max-width:40rem;width:100%;box-sizing:border-box}[role=alert]{border-left:3px solid #a12637;padding-left:.75rem;color:#a12637;overflow-wrap:anywhere}button{min-height:2.5rem}.list-items{padding-left:0;list-style:none}.list-items li{margin:.6rem 0;padding:.6rem;border:1px solid #dde3e8;overflow-wrap:anywhere}.head{display:flex;gap:.4rem;flex-wrap:wrap;align-items:center}.badge{border:1px solid #cbd5de;border-radius:.6rem;padding:0 .5rem;font-size:.85em;color:#3d4f5c}.meta{color:#5b6e7c;margin:.3rem 0}.actions{display:flex;gap:.6rem;flex-wrap:wrap;align-items:center}.toolbar{margin:.5rem 0}.sub{margin-top:.6rem;padding:.5rem;background:#f4f7f9}form.add{display:flex;gap:.6rem;flex-wrap:wrap;align-items:end;margin:1rem 0}form.add label{flex:1 1 10rem}
</style>
