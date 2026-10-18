<script lang="ts">
 import {onMount} from 'svelte';
 import type {MediaDomain,RevisionPolicy,RevisionPolicyMode} from './api.generated';
 import {getRevisionPolicy,updateRevisionPolicy} from './api';
 let domain=$state<MediaDomain>('tv'),saved=$state<RevisionPolicy|null>(null),mode=$state<RevisionPolicyMode>('prefer_and_upgrade'),loading=$state(false),busy=$state(false),uncertain=$state(false),stale=$state(false),dirty=$state(false),error=$state(''),notice=$state('');
 let epoch=0,alive=true;
 async function load(){if(busy||loading)return;loading=true;error='';const version=++epoch,scope=domain;const result=await getRevisionPolicy(scope);if(!alive||version!==epoch)return;loading=false;if(!result.ok){error=result.error;return;}saved=result.data;mode=result.data.mode;dirty=false;uncertain=false;stale=false;notice='Saved revision policy loaded.';}
 async function change(next:MediaDomain){if(next===domain||busy||loading||uncertain||dirty&&!window.confirm('Discard unsaved revision policy changes?'))return;domain=next;saved=null;notice='';await load();}
 async function save(){if(!saved||busy||loading||uncertain||stale)return;busy=true;error='';notice='';const result=await updateRevisionPolicy(domain,{revision:saved.revision,mode});if(!alive)return;busy=false;if(!result.ok){error=result.error;uncertain=result.status===undefined||result.status>=500;stale=result.status===409;return;}saved=result.data;mode=result.data.mode;dirty=false;notice='Revision policy saved.';}
 onMount(()=>{void load();return()=>{alive=false;++epoch;};});
</script>
<section aria-label="Revision policy"><h2>Propers and repacks</h2><nav aria-label="Revision policy media"><button disabled={busy||loading||uncertain} aria-pressed={domain==='tv'} onclick={()=>change('tv')}>TV</button><button disabled={busy||loading||uncertain} aria-pressed={domain==='movies'} onclick={()=>change('movies')}>Movies</button></nav>
<p>This controls revision preference. “Prefer without automatic upgrades” is not a blanket ban on proper releases; other release rules still apply.</p>
{#if loading}<p role="status">Loading revision policy…</p>{/if}{#if busy}<p role="status">Saving revision policy…</p>{/if}{#if error}<p role="alert">{error}</p>{/if}{#if notice}<p role="status">{notice}</p>{/if}
{#if uncertain||stale}<p role="alert">{uncertain?'Write outcome unknown.':'Revision policy changed in another request.'} Your draft is retained. Reload saved policy before another write.</p>{/if}
<button disabled={busy||loading} onclick={()=>{if(!dirty||window.confirm('Replace this draft with the saved revision policy?'))void load();}}>Reload revision policy</button>
<form onsubmit={event=>{event.preventDefault();void save();}}><fieldset disabled={!saved||busy||loading||uncertain||stale}><legend>{domain==='tv'?'TV':'Movie'} revision preference</legend><label>Propers and repacks<select aria-label="Propers and repacks" bind:value={mode} onchange={()=>dirty=true}><option value="prefer_and_upgrade">Prefer and upgrade</option><option value="do_not_upgrade">Prefer without automatic upgrades</option><option value="do_not_prefer">Do not prefer</option></select></label><button>Save revision policy</button></fieldset></form></section>
