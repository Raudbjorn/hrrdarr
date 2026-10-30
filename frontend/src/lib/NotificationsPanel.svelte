<script lang="ts">
  import {onMount} from 'svelte';
  import type {Notification,NotificationImplementation,Tag,NotificationDelivery,MediaDomain} from './api.generated';
  import {listNotifications,getNotificationSchema,createNotification,updateNotification,deleteNotification,testNotificationDraft,testSavedNotification,listTags,listNotificationDeliveries,type SettingsWriteState} from './api';
  import {blankDraft,draftFromNotification,buildInput,describeTest,triggersOn,SCOPE_DEFS,type NotificationDraft} from './notification-draft';

  // The tag catalog version comes from Settings -> Tags: a tag created there must reach the
  // scope choices here without a page reload.
  let { tagVersions = {tv:0,movies:0} }: { tagVersions?: Record<MediaDomain, number> } = $props();

  let schema=$state<NotificationImplementation[]>([]),items=$state<Notification[]>([]),tags=$state<{tv:Tag[];movies:Tag[]}>({tv:[],movies:[]});
  let deliveries=$state<NotificationDelivery[]>([]);
  let draft=$state<NotificationDraft|null>(null),editing=$state<Notification|null>(null);
  let loading=$state(false),write=$state<SettingsWriteState>('idle'),error=$state(''),notice=$state(''),testing=$state(false),testNote=$state('');
  let alive=true,epoch=0;
  const busy=$derived(write==='busy'||testing);
  const impl=$derived(draft?schema.find(s=>s.implementation===draft!.implementation)??null:null);

  async function load() {
    // A superseded read is dropped: a catalog refresh must not overwrite newer state.
    const token=++epoch;
    loading=true;error='';
    const [s,l,tv,mv,d]=await Promise.all([getNotificationSchema(),listNotifications(),listTags('tv'),listTags('movies'),listNotificationDeliveries(10)]);
    if(!alive||token!==epoch)return;
    loading=false;
    if(!s.ok){error=s.error;return;}
    if(!l.ok){error=l.error;return;}
    schema=s.data.items;items=l.data.items;
    tags={tv:tv.ok?tv.data:[],movies:mv.ok?mv.data:[]};
    deliveries=d.ok?d.data.items:[];
  }
  // A tag created in Settings -> Tags bumps the catalog version: reload through the same path
  // used after a write, so the scope choices list the new tag. The draft in progress is kept.
  let seenTagVersions:{tv:number,movies:number}|null=null;
  $effect(()=>{
    const tv=tagVersions.tv,movies=tagVersions.movies;
    if(!seenTagVersions){seenTagVersions={tv,movies};return;}
    if(tv===seenTagVersions.tv&&movies===seenTagVersions.movies)return;
    seenTagVersions={tv,movies};
    void load();
  });
  function startAdd(name:string) {
    const found=schema.find(s=>s.implementation===name);
    if(!found||busy)return;
    editing=null;draft=blankDraft(found);notice='';error='';testNote='';
  }
  function startEdit(n:Notification) {
    const found=schema.find(s=>s.implementation===n.implementation);
    if(!found||busy)return;
    editing=n;draft=draftFromNotification(n,found);notice='';error='';testNote='';
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
    const result=editing?await updateNotification(editing.id,input):await createNotification(input);
    if(!alive)return;
    if(!result.ok){write=result.status===undefined||result.status>=500?'uncertain':'idle';error=result.error;return;}
    write='idle';notice=`Notification "${result.data.name}" saved.`;draft=null;editing=null;await load();
  }
  async function runTest() {
    if(!draft||busy)return;
    const input=built();
    if(!input)return;
    testing=true;error='';testNote='';
    const result=await testNotificationDraft({draft:input,...(editing?{existing_id:editing.id}:{})});
    if(!alive)return;
    testing=false;
    if(!result.ok){error=result.error;return;}
    testNote=describeTest(result.data);
  }
  async function testSaved(n:Notification) {
    if(busy)return;
    testing=true;error='';notice='';
    const result=await testSavedNotification(n.id);
    if(!alive)return;
    testing=false;
    if(!result.ok){error=result.error;return;}
    notice=`${n.name}: ${describeTest(result.data)}`;
  }
  async function remove(n:Notification) {
    if(busy||!window.confirm(`Delete notification "${n.name}"? Pending deliveries for it are discarded.`))return;
    write='busy';error='';notice='';
    const result=await deleteNotification(n.id);
    if(!alive)return;
    write='idle';
    if(!result.ok){error=result.error;return;}
    notice=`Notification "${n.name}" deleted.`;await load();
  }
  function toggleTag(list:number[],id:number) {return list.includes(id)?list.filter(t=>t!==id):[...list,id];}
  function summary(n:Notification) {
    const parts=[];
    if(n.tv)parts.push(`TV ${triggersOn(n.tv.triggers as Record<string,boolean>)} triggers${n.tv.tag_ids.length?`, ${n.tv.tag_ids.length} tags`:''}`);
    if(n.movies)parts.push(`Movies ${triggersOn(n.movies.triggers as Record<string,boolean>)} triggers${n.movies.tag_ids.length?`, ${n.movies.tag_ids.length} tags`:''}`);
    return parts.join('; ');
  }
  onMount(()=>{void load();return()=>{alive=false;};});
</script>
<section aria-label="Notifications" aria-busy={loading||busy}>
  <h2>Connect (notifications)</h2>
  <p>Notifications and media-server updates are sent from the durable event outbox, with retries. Secrets are write-only: they are stored encrypted and never shown again.</p>
  {#if loading}<p role="status">Loading notifications…</p>{/if}
  {#if error}<p role="alert">{error}</p>{/if}
  {#if notice}<p role="status">{notice}</p>{/if}
  {#if write==='uncertain'}<p role="alert">Write outcome unknown. Reload before another write.</p>{/if}
  {#if !draft}
    <ul class="notification-list" aria-label="Configured notifications">
      {#each items as n (n.id)}
        <li>
          <strong>{n.name}</strong> <span>{schema.find(s=>s.implementation===n.implementation)?.label??n.implementation}</span>
          <span>{n.enabled?'Enabled':'Disabled'}</span> <span>{summary(n)}</span>
          <button disabled={busy} onclick={()=>startEdit(n)} aria-label={`Edit ${n.name}`}>Edit</button>
          <button disabled={busy} onclick={()=>testSaved(n)} aria-label={`Test ${n.name}`}>Test</button>
          <button disabled={busy} onclick={()=>remove(n)} aria-label={`Delete ${n.name}`}>Delete</button>
        </li>
      {:else}
        {#if !loading}<li>No notifications are configured.</li>{/if}
      {/each}
    </ul>
    <label>Add notification
      <select disabled={busy||loading} onchange={e=>{if(e.currentTarget.value)startAdd(e.currentTarget.value);}} aria-label="Add notification">
        <option value="">Choose an implementation…</option>
        {#each schema as s (s.implementation)}<option value={s.implementation}>{s.label}</option>{/each}
      </select>
    </label>
    <section aria-label="Recent deliveries">
      <h3>Recent deliveries</h3>
      <ul>{#each deliveries as d (d.id)}<li>{d.event_kind}{d.media_type?` (${d.media_type})`:''}: {d.state}, {d.attempts} attempt{d.attempts===1?'':'s'}{d.last_error?`, ${d.last_error}`:''}</li>{:else}<li>No deliveries yet.</li>{/each}</ul>
    </section>
  {:else if impl}
    <form onsubmit={e=>{e.preventDefault();void save();}} aria-label={editing?`Edit ${editing.name}`:`Add ${impl.label}`}>
      <fieldset disabled={busy}>
        <legend>{editing?'Edit':'Add'} {impl.label}</legend>
        <p>{impl.description}</p>
        <label>Name<input bind:value={draft.name} autocomplete="off" required/></label>
        <label class="inline"><input type="checkbox" bind:checked={draft.enabled}/> Enabled</label>
        {#each impl.fields as f (f.name)}
          {#if f.secret}
            <label>{f.label}{f.required?' *':''}
              <input type="password" autocomplete="new-password" placeholder={editing?.secrets_configured.includes(f.name)?'Configured (leave blank to keep)':''} bind:value={draft.secrets[f.name]}/>
            </label>
            {#if editing?.secrets_configured.includes(f.name)&&!f.required}<label class="inline"><input type="checkbox" checked={draft.cleared.includes(f.name)} onchange={()=>draft!.cleared=draft!.cleared.includes(f.name)?draft!.cleared.filter(c=>c!==f.name):[...draft!.cleared,f.name]}/> Clear {f.label}</label>{/if}
          {:else if f.kind==='boolean'}
            <label class="inline"><input type="checkbox" checked={draft.settings[f.name]===true} onchange={e=>draft!.settings[f.name]=e.currentTarget.checked}/> {f.label}</label>
          {:else if f.kind==='select'}
            <label>{f.label}<select bind:value={draft.settings[f.name]}>{#each f.options as o (o)}<option value={o}>{o}</option>{/each}</select></label>
          {:else if f.kind==='number'}
            <label>{f.label}{f.required?' *':''}<input type="number" min={f.min??undefined} max={f.max??undefined} step="1" bind:value={draft.settings[f.name]}/></label>
          {:else}
            <label>{f.label}{f.required?' *':''}<input type="text" autocomplete="off" spellcheck="false" bind:value={draft.settings[f.name]}/></label>
          {/if}
          {#if f.help}<small>{f.help}</small>{/if}
        {/each}
        {#each SCOPE_DEFS as {key,label,defs} (key)}
          {#if impl.domains.includes(key as 'tv'|'movies')}
            {@const scope=key==='tv'?draft.tv:draft.movies}
            <fieldset class="scope" aria-label={`${label} scope`}>
              <legend><label class="inline"><input type="checkbox" bind:checked={scope.on}/> {label}</label></legend>
              {#if scope.on}
                {#each defs as def (def.key)}
                  <label class="inline"><input type="checkbox" checked={(scope.triggers as Record<string,boolean>)[def.key]===true} onchange={e=>(scope.triggers as Record<string,boolean>)[def.key]=e.currentTarget.checked}/> {def.label}</label>
                {/each}
                <div role="group" aria-label={`${label} tags`}>
                  <small>Tags (none means all {key==='tv'?'series':'movies'})</small>
                  {#each tags[key as 'tv'|'movies'] as t (t.id)}
                    <label class="inline"><input type="checkbox" checked={scope.tagIds.includes(t.id)} onchange={()=>scope.tagIds=toggleTag(scope.tagIds,t.id)}/> {t.label}</label>
                  {:else}<small>No {label} tags exist.</small>{/each}
                </div>
              {/if}
            </fieldset>
          {/if}
        {/each}
        <div class="actions">
          <button type="submit">Save</button>
          <button type="button" onclick={runTest}>Test</button>
          <button type="button" onclick={cancel}>Cancel</button>
        </div>
        {#if testing}<p role="status">Sending test…</p>{/if}
        {#if testNote}<p role="status" aria-label="Test result">{testNote}</p>{/if}
      </fieldset>
    </form>
  {/if}
</section>
<style>
  section{margin-top:1.5rem}fieldset{display:grid;gap:.6rem;margin-top:1rem;border:1px solid #cbd5de;padding:1rem;min-width:0}label{display:grid;gap:.2rem}label.inline{display:flex;gap:.4rem;align-items:center}input[type=text],input[type=password],input[type=number],select{max-width:40rem;box-sizing:border-box}[role=alert]{border-left:3px solid #a12637;padding-left:.75rem;color:#a12637}button{min-height:2.5rem}.notification-list li{margin:.4rem 0}.notification-list span{color:#5b6e7c;margin:0 .4rem}.actions{display:flex;gap:.6rem}
</style>
