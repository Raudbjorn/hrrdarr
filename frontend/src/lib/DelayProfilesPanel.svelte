<script lang="ts">
 import {untrack} from 'svelte';
 import type {MediaDomain,DelayProfileCatalog,DelayProfileInput,Tag,DelayProfile} from './api.generated';
 import type {SettingsWriteState,Result} from './api';
 import {listDelayProfiles,getDelayProfileSchema,listTags,createDelayProfile,updateDelayProfile,deleteDelayProfile,reorderDelayProfiles,saveReleasePolicy} from './api';
 import {draftFrom,draftError,draftInput,integer,moveProfile,type DelayDraft} from './delay-profile-draft';
 let {tagVersions,relatedWrites,onwrite,onchange,request=null}:{tagVersions:Record<MediaDomain,number>;relatedWrites:Record<MediaDomain,SettingsWriteState>;onwrite:(domain:MediaDomain,state:SettingsWriteState)=>void;onchange:(domain:MediaDomain)=>void;request?:{domain:MediaDomain;id?:number;nonce:number}|null}=$props();
 let domain=$state<MediaDomain>('tv'),catalog=$state<DelayProfileCatalog|null>(null),schema=$state<DelayProfileInput|null>(null),tags=$state<Tag[]>([]),draft=$state<DelayDraft|null>(null),id=$state<number|null>(null),global=$state(false),order=$state<number[]>([]),baseline=$state(0);
 let loading=$state(false),busy=$state(false),uncertain=$state(false),stale=$state(false),dirty=$state(false),orderDirty=$state(false),deleting=$state<DelayProfile|null>(null),error=$state(''),notice=$state('');
 let torrent=$state('0'),usenet=$state('0'),days=$state('0'),compatReady=$state(false),compatDirty=$state(false),compatStale=$state(false);
 let catalogTagVersion=$state(-1);
 let epoch=0,openEpoch=0,initialized=false,seenRequest=0,seenTags={tv:0,movies:0};
 const labelsStale=$derived(catalog!==null&&catalogTagVersion!==tagVersions[domain]);
 const blocked=$derived(busy||loading||uncertain||labelsStale||relatedWrites[domain]!=='idle');
 $effect(()=>{const revision=tagVersions[domain];untrack(()=>{if(revision!==seenTags[domain]){seenTags[domain]=revision;deleting=null;if(draft)stale=true;}});});
 $effect(()=>{if(relatedWrites[domain]!=='idle')deleting=null;});
 function discard(){return !(dirty||orderDirty||compatDirty)||window.confirm('Discard unsaved delay settings changes?');}
 function accept(value:DelayProfileCatalog,preserveOrder=false){catalog=value;if(preserveOrder)return;order=value.profiles.filter(p=>!p.is_global).map(p=>p.id);orderDirty=false;}
 function fillCompatibility(value:DelayProfileCatalog){const fallback=value.profiles.find(p=>p.is_global);torrent=String(fallback?.settings.torrent_delay_minutes??0);usenet=String(fallback?.settings.usenet_delay_minutes??0);days=String(value.availability_delay_days??0);compatReady=true;compatDirty=false;compatStale=false;}
 async function load(replace=false){
  if(busy||loading||relatedWrites[domain]!=='idle')return;deleting=null;const token=++epoch,scope=domain,tagVersion=tagVersions[scope];loading=true;error='';
  const [result,defaults,labels]=await Promise.all([listDelayProfiles(scope),getDelayProfileSchema(scope),listTags(scope)]);if(token!==epoch)return;loading=false;
  if(!result.ok||!defaults.ok||!labels.ok){error=[result,defaults,labels].filter(r=>!r.ok).map(r=>r.ok?'':r.error).join(' ');return;}
  if(tagVersion!==tagVersions[scope]||relatedWrites[scope]!=='idle'){error='Tag catalog changed during loading. Reload before editing.';return;}
  const changed=catalog!==null&&catalog.revision!==result.data.revision;
  if(changed){if(draft||orderDirty)stale=true;if(compatReady)compatStale=true;}
  accept(result.data,!replace&&orderDirty);schema=defaults.data;tags=labels.data;catalogTagVersion=tagVersion;
  if(replace){draft=null;id=null;dirty=false;stale=false;fillCompatibility(result.data);}else if(!compatReady)fillCompatibility(result.data);
  uncertain=false;onwrite(scope,'idle');notice='Catalog loaded. Reopen a profile to use the current revision.';
 }
 async function open(scope:MediaDomain,profileId?:number){
  if(busy||uncertain||relatedWrites[scope]!=='idle'){error='Finish or reconcile the current write before opening another delay profile.';return;}if(!discard())return;
  const opened=++openEpoch;++epoch;loading=false;domain=scope;catalog=null;schema=null;tags=[];draft=null;id=null;global=false;stale=false;order=[];dirty=false;orderDirty=false;compatDirty=false;compatReady=false;notice='';await load(true);if(opened!==openEpoch||scope!==domain||error)return;if(profileId!==undefined&&catalog)edit(profileId);
 }
 $effect(()=>{const target=request;untrack(()=>{if(!initialized){initialized=true;seenRequest=target?.nonce??0;void open(target?.domain??'tv',target?.id);}else if(target&&target.nonce!==seenRequest){seenRequest=target.nonce;void open(target.domain,target.id);}});});
 function edit(profileId:number){if(blocked||!catalog||!schema||!discard())return;const profile=catalog.profiles.find(p=>p.id===profileId);if(!profile){error='Delay profile no longer exists. Reload the catalog.';return;}deleting=null;id=profile.id;global=profile.is_global;draft=profile.settings.semantics==='profile'?draftFrom(schema,profile):null;baseline=catalog.revision;stale=false;dirty=false;orderDirty=false;order=catalog.profiles.filter(p=>!p.is_global).map(p=>p.id);notice=profile.settings.semantics==='legacy_age_only'?'Legacy fields remain unknown until you explicitly configure a full policy.':'';}
 function configure(){if(blocked||!catalog||!schema||id===null)return;const profile=catalog.profiles.find(p=>p.id===id);if(!profile)return;draft=draftFrom(schema,profile);baseline=catalog.revision;stale=false;dirty=true;}
 function create(){if(blocked||!catalog?.configured||!schema||!discard())return;draft=draftFrom(schema);id=null;global=false;baseline=catalog.revision;stale=false;dirty=true;deleting=null;}
 function failure(result:{status?:number;error:string}){error=result.error;uncertain=result.status===undefined||result.status>=500;stale=true;compatStale=true;onwrite(domain,uncertain?'uncertain':'idle');if(uncertain)onchange(domain);}
 async function mutate(action:()=>Promise<Result<DelayProfileCatalog>>,message:string){
  if(blocked)return;busy=true;error='';notice='';deleting=null;onwrite(domain,'busy');const result=await action();busy=false;
  if(!result.ok){failure(result);return;}
  accept(result.data);draft=null;id=null;dirty=false;stale=false;compatStale=true;onwrite(domain,'idle');onchange(domain);notice=message;
 }
 async function save(){if(blocked||stale||!draft||!catalog)return;error=draftError(draft,global,tags,catalog.profiles,id);if(error)return;const body={revision:baseline,profile:draftInput(draft)};await mutate(()=>id===null?createDelayProfile(domain,body):updateDelayProfile(domain,id!,body),'Delay profile saved.');}
 async function saveOrder(){if(blocked||stale||!catalog||!orderDirty)return;if(dirty&&!window.confirm('Discard profile edits and save the new order?'))return;const value={revision:catalog.revision,ids:[...order]};await mutate(()=>reorderDelayProfiles(domain,value),'Delay order saved.');}
 async function remove(){if(blocked||stale||!deleting||!catalog)return;const target=deleting;await mutate(()=>deleteDelayProfile(domain,target.id,catalog!.revision),'Delay profile deleted.');}
 async function reloadCompatibility(){if(blocked)return;if(compatDirty&&!window.confirm('Replace the compatibility draft with saved minutes and availability?'))return;await load();if(catalog&&!error){fillCompatibility(catalog);if(draft)stale=true;}}
 async function saveCompatibility(){
  if(blocked||!compatReady||compatStale)return;const t=integer(torrent,0,10080),u=integer(usenet,0,10080),d=integer(days,-365,365);if(t===null||u===null||d===null){error='Use whole minutes 0–10080 and availability days −365–365.';return;}
  busy=true;deleting=null;error='';notice='';onwrite(domain,'busy');const result=await saveReleasePolicy(domain,{torrent_delay_minutes:t,usenet_delay_minutes:u,availability_delay_days:domain==='tv'?0:d});busy=false;
  if(!result.ok){failure(result);return;}onwrite(domain,'idle');onchange(domain);compatDirty=false;compatStale=false;if(draft)stale=true;await load();if(catalog&&!error)fillCompatibility(catalog);notice='Release delay settings saved.';
 }
 const label=(profile:DelayProfile)=>profile.is_global?'Global fallback':`Profile ${profile.id}: ${profile.tag_ids.map(id=>tags.find(t=>t.id===id)?.label??`Unknown tag ${id}`).join(', ')}`;
</script>
<section aria-label="Delay profiles"><h2>Delay profiles</h2><nav aria-label="Delay profile media"><button disabled={busy||loading||uncertain||relatedWrites[domain]!=='idle'} aria-pressed={domain==='tv'} onclick={()=>open('tv')}>TV</button><button disabled={busy||loading||uncertain||relatedWrites[domain]!=='idle'} aria-pressed={domain==='movies'} onclick={()=>open('movies')}>Movies</button></nav>
<p>The first matching tagged profile applies; the global fallback is last. Protocol enablement and preferred protocol are separate settings.</p>
{#if loading}<p role="status">Loading delay profiles…</p>{/if}{#if busy}<p role="status">Saving delay settings…</p>{/if}{#if error}<p role="alert">{error}</p>{/if}{#if notice}<p role="status">{notice}</p>{/if}
{#if uncertain}<p role="alert">Write outcome unknown. Keep this draft and reload the full catalog before another write. A create may already have succeeded; select its saved row instead of creating it again.</p>{/if}
{#if stale||labelsStale}<p role="alert">Draft is stale. Reload, review the saved catalog, then explicitly reopen the profile before saving.</p>{/if}
{#if relatedWrites[domain]!=='idle'}<p role="status">Finish or reconcile tag settings before changing delay profiles.</p>{/if}
<button disabled={busy||loading||relatedWrites[domain]!=='idle'} onclick={()=>load()}>Reload delay catalog</button><button disabled={blocked||!catalog?.configured} onclick={create}>New tagged delay profile</button>
{#if catalog}
 <p>{catalog.configured?'Background delay policy is configured.':'Not configured. Saving the global fallback explicitly activates background delay settings.'} Availability adjustment: {catalog.availability_delay_days??'unknown'} days.</p>
 <ul>{#each [...order.map(id=>catalog!.profiles.find(p=>p.id===id)!).filter(Boolean),...catalog.profiles.filter(p=>p.is_global)] as profile(profile.id)}<li><strong>{label(profile)}</strong><p>{profile.settings.semantics==='legacy_age_only'?'Legacy age-only: enabled/preferred protocols and bypass settings unknown.':`Preferred ${profile.settings.preferred_protocol}; torrent ${profile.settings.enable_torrent?'enabled':'disabled'}, Usenet ${profile.settings.enable_usenet?'enabled':'disabled'}.`} Torrent: {profile.settings.torrent_delay_minutes??'unknown'} minutes; Usenet: {profile.settings.usenet_delay_minutes??'unknown'} minutes.</p>
 <button disabled={blocked} onclick={()=>edit(profile.id)}>Edit delay profile {profile.id}</button>{#if !profile.is_global}<button disabled={blocked||stale||order.indexOf(profile.id)===0} onclick={()=>{order=moveProfile(order,profile.id,-1);orderDirty=true;deleting=null;}}>Move delay profile {profile.id} up</button><button disabled={blocked||stale||order.indexOf(profile.id)===order.length-1} onclick={()=>{order=moveProfile(order,profile.id,1);orderDirty=true;deleting=null;}}>Move delay profile {profile.id} down</button><button disabled={blocked||stale} onclick={()=>{if(discard())deleting=profile;}}>Delete delay profile {profile.id}</button>{/if}</li>{/each}</ul>
 <button disabled={blocked||stale||!orderDirty} onclick={saveOrder}>Save delay order</button>
{/if}
{#if id!==null&&!draft}<p>Unspecified protocol and bypass values are unknown, not disabled. Configuring proposes schema defaults and preserves known delay minutes.</p><button disabled={blocked} onclick={configure}>Configure full delay policy</button>{/if}
{#if draft}<form onsubmit={event=>{event.preventDefault();void save();}} oninput={()=>{dirty=true;deleting=null;}}><fieldset disabled={blocked||stale}><legend>{global?'Global fallback':id===null?'New tagged delay profile':`Delay profile ${id}`}</legend>
<label><input type="checkbox" bind:checked={draft.enable_torrent}/>Enable Torrent</label><label><input type="checkbox" bind:checked={draft.enable_usenet}/>Enable Usenet</label>
<label>Preferred protocol<select aria-label="Preferred protocol" bind:value={draft.preferred_protocol}><option value="usenet">Usenet</option><option value="torrent">Torrent</option></select></label>
<label>Torrent delay minutes<input inputmode="numeric" bind:value={draft.torrent}/></label><label>Usenet delay minutes<input inputmode="numeric" bind:value={draft.usenet}/></label>
<label><input type="checkbox" bind:checked={draft.bypass_if_highest_quality}/>Bypass delay at highest allowed quality</label><label><input type="checkbox" bind:checked={draft.bypass_if_above_custom_format_score}/>Bypass delay at minimum custom format score</label><label>Minimum custom format score<input inputmode="numeric" bind:value={draft.score}/></label><p>These bypasses require the preferred protocol. Highest allowed quality is distinct from the quality cutoff.</p>
{#if !global}<fieldset><legend>Delay profile tags</legend>{#each tags as tag(tag.id)}<label><input type="checkbox" value={tag.id} bind:group={draft.tag_ids}/>{tag.label}{catalog?.profiles.some(p=>p.id!==id&&p.tag_ids.includes(tag.id))?' (used by another profile)':''}</label>{:else}<p>No tags; create tags in Tags settings first.</p>{/each}{#each draft.tag_ids.filter(id=>!tags.some(tag=>tag.id===id)) as missing}<p role="alert">Unknown tag {missing}; reopen after reviewing the catalog.</p>{/each}</fieldset>{/if}
<button>Save delay profile</button></fieldset></form>{/if}
{#if deleting}<div role="group" aria-label="Confirm delay profile deletion"><p>Delete {label(deleting)}? Owners will use another matching profile or the global fallback.</p><button disabled={blocked||stale} onclick={remove}>Confirm delete delay profile</button><button disabled={busy} onclick={()=>deleting=null}>Cancel delay deletion</button></div>{/if}
<details><summary>Compatibility minutes and availability</summary><p>This legacy editor updates global minutes and movie availability, preserving full protocol/bypass fields and tagged profiles. Other clients can overwrite these values: the compatibility endpoint has no revision check.</p>{#if compatStale}<p role="alert">Compatibility draft is stale; read saved release delays before saving.</p>{/if}
<form onsubmit={event=>{event.preventDefault();void saveCompatibility();}} oninput={()=>compatDirty=true}><fieldset disabled={blocked||!compatReady||compatStale}><legend>Background release delays</legend><label>Torrent delay (minutes)<input bind:value={torrent} inputmode="numeric"/></label><label>Usenet delay (minutes)<input bind:value={usenet} inputmode="numeric"/></label>{#if domain==='movies'}<label>Availability adjustment (days)<input bind:value={days} inputmode="numeric"/></label>{/if}<button>Save release delays</button></fieldset></form><button disabled={blocked} onclick={reloadCompatibility}>Read saved release delays</button></details>
</section>
