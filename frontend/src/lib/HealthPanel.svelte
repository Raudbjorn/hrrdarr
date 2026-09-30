<script lang="ts">
  import {onMount,untrack} from 'svelte';
  import {getHealth,listHealthCommands,getHealthCommand,createHealthCommand,cancelHealthCommand,getHealthTransitions} from './api';
  import type {ApiPage,HealthSnapshot,HealthCommand,HealthAdmission,HealthTransitions,HealthScope,HealthIdentity,CommandStatus,MediaDomain} from './api.generated';
  let {active=true,onsettings=()=>{}}:{active?:boolean;onsettings?:(domain:MediaDomain)=>void}=$props();
  let scope=$state<HealthScope>('all'),offset=$state(0),historyOffset=$state(0),status=$state<CommandStatus|''>('');
  let snapshot=$state<HealthSnapshot|null>(null),history=$state<ApiPage<HealthCommand>|null>(null),detail=$state<HealthCommand|null>(null),detailId=$state('');
  let transitions=$state<HealthTransitions|null>(null),cursor=$state(0),gap=$state(false);
  let snapshotError=$state(''),historyError=$state(''),detailError=$state(''),transitionError=$state(''),mutationError=$state(''),notice=$state('');
  let reading=$state(false),mutating=$state(false),unknown=$state<'admission'|'cancel'|null>(null),unknownId=$state('');
  // ponytail: eight retained manual requests; explicitly dismiss reviewed tracking before adding more.
  let requests=$state<{number:number;admission:HealthAdmission}[]>([]),requestNumber=0;
  let alive=false,version=0,timer:ReturnType<typeof setTimeout>|undefined;
  const identity=(value:HealthIdentity)=>`${value.scope}:${value.check_key}`;
  const date=(value:number|null)=>value===null?'Not recorded':new Date(value*1000).toLocaleString();
  const terminal=(command:HealthCommand)=>['succeeded','failed','cancelled'].includes(command.status);
  const safeLink=(value:string)=>/^https?:\/\//i.test(value)?value:null;
  const issueFor=(key:HealthIdentity)=>snapshot?.issues.find(issue=>identity(issue.identity)===identity(key));
  function tokenStatus(admission:HealthAdmission){
    if(!snapshot||snapshotError)return 'Unverified — refresh the requested checks.';
    return admission.selection.every(token=>snapshot!.checks.some(check=>identity(check.identity)===identity(token.identity)&&check.observed_generation!==null&&check.observed_generation>=token.generation&&check.observed_epoch===snapshot!.lifecycle.epoch&&check.evaluation==='current'&&!check.last_error))
      ? 'Requested generations observed in the current epoch.' : 'Not yet verified on this page — pending, stale, failed, or outside this scope/page.';
  }
  function schedule(){clearTimeout(timer);if(alive&&active&&!document.hidden)timer=setTimeout(()=>void refresh(),5000);}
  async function refresh(){
    if(!alive||!active||document.hidden||reading||mutating)return;
    clearTimeout(timer);reading=true;const ticket=version,id=detailId;
    const [health,commands,events,selected]=await Promise.all([getHealth(scope,offset),listHealthCommands(scope,historyOffset,status),getHealthTransitions(cursor),id?getHealthCommand(id):Promise.resolve(null)]);
    reading=false;
    if(!alive)return;
    if(ticket!==version||!active||document.hidden){if(active&&!document.hidden)void refresh();return;}
    if(health.ok){snapshot=health.data;snapshotError='';}else snapshotError=health.error;
    if(commands.ok){history=commands.data;historyError='';}else historyError=commands.error;
    if(events.ok){transitions=events.data;gap=gap||events.data.gap;transitionError='';}else transitionError=events.error;
    if(selected){
      if(selected.ok){detail=selected.data;detailError='';if(unknown==='cancel'&&unknownId===id&&terminal(selected.data)){notice=`Cancellation outcome was unknown; recorded batch now ${selected.data.status}. This is readback, not a replay.`;unknown=null;unknownId='';}}
      else detailError=selected.status===404?'Batch unknown or pruned; cancellation is not confirmed.':selected.error;
    }
    schedule();
  }
  function changed(){++version;clearTimeout(timer);void refresh();}
  function changeScope(next:HealthScope){scope=next;offset=0;historyOffset=0;snapshot=null;history=null;changed();}
  function inspect(command:HealthCommand){if(mutating)return;detailId=command.id;detail=command;detailError='';changed();}
  async function admit(){
    if(reading||mutating||unknown||requests.length>=8)return;
    mutating=true;clearTimeout(timer);mutationError='';notice='';const result=await createHealthCommand(scope);mutating=false;
    if(!alive)return;
    if(result.ok){requests=[...requests,{number:++requestNumber,admission:result.data}];notice=`Health request ${result.data.outcome.replaceAll('_',' ')}. Requested generations require their own observation.`;detailId=result.data.outcome==='pending'?result.data.active_command_id:result.data.command_id;detail=null;}
    else{mutationError=`${result.error}${result.code?` (${result.code})`:''}`;if(result.status===undefined||result.status>=500)unknown='admission';}
    changed();
  }
  async function cancel(command:HealthCommand){
    if(reading||mutating||unknown)return;
    if(!window.confirm(`Cancel entire health batch ${command.id}?\nMembers: ${command.members.map(member=>identity(member.identity)).join(', ')}.\nThis also cancels coalesced requests and automatic work, including any members added before cancellation. Previous observations and newer pending generations remain. Choose Cancel to keep running.`))return;
    mutating=true;clearTimeout(timer);mutationError='';notice='';detailId=command.id;
    const result=await cancelHealthCommand(command.id);mutating=false;if(!alive)return;
    if(result.ok){detail=result.data;notice=`Batch ${command.id}: ${result.data.status}. Previous observations and newer pending generations remain.`;}
    else{mutationError=`${result.error}${result.code?` (${result.code})`:''}`;if(result.status===undefined||result.status>=500){unknown='cancel';unknownId=command.id;}}
    changed();
  }
  $effect(()=>{const shown=active;untrack(()=>{++version;clearTimeout(timer);if(shown)void refresh();});});
  onMount(()=>{alive=true;void refresh();const visibility=()=>{++version;clearTimeout(timer);if(!document.hidden&&active)void refresh();};document.addEventListener('visibilitychange',visibility);return()=>{alive=false;++version;clearTimeout(timer);document.removeEventListener('visibilitychange',visibility);};});
</script>

<section aria-labelledby="health-heading" aria-busy={reading||mutating}>
  <h1 id="health-heading">Health</h1>
  <p>Cached observations of registered checks. Coverage is incomplete; this is not a full system health guarantee. Reading this page never starts a check.</p>
  <div class="controls"><label>Health scope<select value={scope} onchange={event=>changeScope(event.currentTarget.value as HealthScope)} disabled={mutating}><option value="all">All registered scopes</option><option value="tv">TV</option><option value="movies">Movies</option><option value="system">System</option></select></label><button disabled={reading||mutating} onclick={()=>void refresh()}>Refresh health</button><button disabled={reading||mutating||!!unknown||requests.length>=8} onclick={admit}>Run {scope} health checks</button></div>
  {#if mutating}<p role="status">Submitting health request…</p>{/if}
  {#if notice}<p role="status">{notice}</p>{/if}
  {#if mutationError}<p role="alert">{mutationError}</p>{/if}
  {#if unknown}<div role="alert"><p>Outcome unknown; inspect recorded work before requesting again. No automatic mutation retry is performed.</p>{#if unknown==='admission'}<p>The lost response had no confirmed generation tokens; later snapshots cannot identify that request.</p>{/if}<button disabled={reading||mutating} onclick={()=>void refresh()}>Inspect recorded work</button><button disabled={reading||mutating} onclick={()=>{unknown=null;unknownId='';notice='A further manual request will be new work, not a retry of the unknown request.';}}>I reviewed recorded work; allow a new request</button></div>{/if}
  {#if requests.length}<section aria-label="Manual health requests"><h2>Manual requests</h2><p>Tracking survives workspace navigation, but ends when this page is reloaded. Batch success alone does not establish that a later request was covered.</p>{#each requests as request (request.number)}<article><h3>Request {request.number}: {request.admission.requested_scope} — {request.admission.outcome.replaceAll('_',' ')}</h3><p>{tokenStatus(request.admission)}</p><ul>{#each request.admission.selection as token (identity(token.identity))}<li>{identity(token.identity)}: requested generation {token.generation}</li>{/each}</ul><button onclick={()=>requests=requests.filter(item=>item.number!==request.number)}>Dismiss request {request.number} tracking</button></article>{/each}<p>Dismissing tracking does not cancel any work.</p></section>{/if}
  {#if snapshotError}<p role="alert">Health read failed: {snapshotError}. Retained observations may be stale.</p>{/if}
  {#if !snapshot}<p>{reading?'Reading health…':'No health snapshot loaded.'}</p>{:else}
    <section aria-label="Registered check coverage"><h2>{scope} coverage</h2><p>{snapshot.summary.total} registered; {snapshot.summary.current} current; {snapshot.summary.never_run} never evaluated; {snapshot.summary.stale} stale; {snapshot.summary.failed} failed; {snapshot.summary.non_ok} cached non-OK observations. Counters overlap.</p>
    {#if snapshot.summary.total===0}<p>No registered checks in this scope.</p>{:else if !snapshotError&&snapshot.summary.current===snapshot.summary.total&&snapshot.summary.failed===0&&snapshot.summary.non_ok===0}<p>No current issues in registered checks.</p>{/if}
    <p>Registered identities: {snapshot.coverage.identities.map(identity).join(', ')||'None'}</p><p>Pending generations: {snapshot.pending_keys.map(token=>`${identity(token.identity)}=${token.generation}`).join(', ')||'None recorded'}</p>
    {#each snapshot.checks as check (identity(check.identity))}{@const issue=issueFor(check.identity)}<article aria-label={identity(check.identity)}><h3>{identity(check.identity)} — {check.evaluation}</h3><p>{check.checked_at===null?'Never evaluated':`Checked ${date(check.checked_at)}; observed generation ${check.observed_generation}, epoch ${check.observed_epoch}`}</p><p>Current generation {check.generation}; pending reason bits {check.pending_reasons}; due {date(check.due_at)}. Startup: {check.startup?'yes':'no'}; scheduled: {check.scheduled?'yes':'no'}.</p>{#if check.last_error}<p role="alert">Evaluation error: {check.last_error}. Any previous observation is retained.</p>{/if}{#if issue}<p>{issue.severity}: {issue.message} ({issue.reason})</p>{#if safeLink(issue.wiki_url)}<a href={safeLink(issue.wiki_url)} target="_blank" rel="noopener noreferrer">Check help</a>{/if}{:else}<p>No cached issue on this check; evaluation state determines freshness.</p>{/if}{#if check.identity.check_key==='completed_download_handling'&&(check.identity.scope==='tv'||check.identity.scope==='movies')}<button onclick={()=>onsettings(check.identity.scope as MediaDomain)}>Open {check.identity.scope} completed-download settings</button>{/if}</article>{:else}<p>No checks on this page. Scoped summary above covers all registered checks.</p>{/each}
    <div class="controls"><button disabled={mutating||offset===0} onclick={()=>{offset=Math.max(0,offset-16);snapshot=null;changed();}}>Previous checks</button><button disabled={mutating||offset+16>=snapshot.total||offset>=127} onclick={()=>{offset=Math.min(127,offset+16);snapshot=null;changed();}}>Next checks</button><span>Check offset {offset}; up to 16 per page.</span></div></section>
    <section aria-label="Health lifecycle"><h2>Lifecycle</h2><dl><dt>Epoch</dt><dd>{snapshot.lifecycle.epoch}</dd><dt>Started</dt><dd>{date(snapshot.lifecycle.started_at)}</dd><dt>Grace phase</dt><dd>{snapshot.lifecycle.grace_phase}</dd><dt>Grace recheck due</dt><dd>{date(snapshot.lifecycle.grace_due_at)}</dd><dt>Next scheduled checks</dt><dd>{date(snapshot.lifecycle.next_scheduled_at)}</dd><dt>Last successful batch completion</dt><dd>{date(snapshot.lifecycle.last_batch_completed_at)} — selected work only, not all checks.</dd></dl>{#if snapshot.lifecycle.schedule_error}<p role="alert">Scheduler error: {snapshot.lifecycle.schedule_error}</p>{/if}<p>Grace expires only after its successful recheck, not merely when the due time passes.</p></section>
    <section aria-label="Global active health batch"><h2>Global active batch</h2>{#if snapshot.active_command}{@const batch=snapshot.active_command}<p>{batch.id}: {batch.status}. This batch is global, independent of the selected scope.</p><ul>{#each batch.members as member (identity(member.identity))}<li>{identity(member.identity)} — admitted {member.admitted_generation}, captured {member.captured_generation??'not yet'}</li>{/each}</ul><button disabled={mutating} onclick={()=>inspect(batch)}>Inspect active batch</button><button disabled={reading||mutating||!!unknown} onclick={()=>cancel(batch)}>Cancel entire active batch</button>{:else}<p>No active health batch recorded.</p>{/if}</section>
  {/if}
  <section aria-label="Health command history"><h2>Retained command history</h2><p>Membership filtered by {scope}. New work and retention can shift pages.</p><label>Health command status<select value={status} disabled={mutating} onchange={event=>{status=event.currentTarget.value as CommandStatus|'';historyOffset=0;history=null;changed();}}><option value="">All statuses</option>{#each ['queued','running','retry_wait','succeeded','failed','cancelled'] as state}<option value={state}>{state}</option>{/each}</select></label>{#if historyError}<p role="alert">History read failed: {historyError}. Retained history shown.</p>{/if}{#if history}<p>{history.total} matching retained batches.</p><ul>{#each history.items as command (command.id)}<li><button disabled={mutating} onclick={()=>inspect(command)}>Inspect batch {command.id}</button> {command.status} — {command.members.map(member=>identity(member.identity)).join(', ')}</li>{:else}<li>No batches on this page.</li>{/each}</ul><div class="controls"><button disabled={mutating||historyOffset===0} onclick={()=>{historyOffset=Math.max(0,historyOffset-20);history=null;changed();}}>Previous batches</button><button disabled={mutating||historyOffset+20>=history.total||historyOffset>=128} onclick={()=>{historyOffset=Math.min(128,historyOffset+20);history=null;changed();}}>Next batches</button><button disabled={mutating||historyOffset===0} onclick={()=>{historyOffset=0;history=null;changed();}}>Newest batches</button></div>{/if}</section>
  {#if detailId}<section aria-label="Health batch detail"><h2>Batch {detailId}</h2>{#if detailError}<p role="alert">{detailError} Retained detail may be stale.</p>{/if}{#if detail}<p>{detail.status}; priority {detail.priority}; attempts {detail.attempts}; error {detail.error_code??'none recorded'}.</p><p>Created {date(detail.created_at)}; started {date(detail.started_at)}; completed {date(detail.completed_at)}; next attempt {terminal(detail)?'none':date(detail.next_attempt_at)}; epoch {detail.epoch}; grace work {detail.is_grace?'yes':'no'}.</p><ul>{#each detail.members as member (identity(member.identity))}<li>{identity(member.identity)}: admitted generation {member.admitted_generation}; captured {member.captured_generation??'not yet'}; captured reasons {member.captured_reasons??'not yet'}; due {date(member.captured_due_at)}</li>{/each}</ul>{#if !terminal(detail)}<button disabled={reading||mutating||!!unknown||!!detailError} onclick={()=>cancel(detail!)}>Cancel entire inspected batch</button>{/if}{/if}</section>{/if}
  <section aria-label="Health diagnostic transitions"><h2>Diagnostic transitions — all scopes</h2><p>Bounded retained issue/restored events, not delivered notifications or a complete audit trail. Restored events describe the previous issue.</p>{#if gap}<p role="alert">Retention gap observed: earlier transitions are unavailable.</p>{/if}{#if transitionError}<p role="alert">Transition read failed: {transitionError}. Retained events shown.</p>{/if}{#if transitions}<p>Oldest retained: {transitions.oldest_retained_sequence??'none'}; latest: {transitions.latest_sequence??'none'}; showing after {cursor}.</p><ul>{#each transitions.items as event (event.event_id)}<li>#{event.sequence}: {event.kind} — {identity(event.issue.identity)}, {event.issue.severity}: {event.issue.message}; {date(event.created_at)}; {event.in_grace?'in grace':'outside grace'}. Batch {event.command_id}.</li>{:else}<li>No transitions on this page.</li>{/each}</ul><div class="controls"><button disabled={mutating||cursor===0} onclick={()=>{cursor=0;transitions=null;changed();}}>Oldest retained events</button><button disabled={mutating||transitions.latest_sequence===null||transitions.next_cursor>=transitions.latest_sequence} onclick={()=>{cursor=transitions!.next_cursor;transitions=null;changed();}}>Newer events</button></div>{/if}</section>
</section>
<style>
  section{margin-block:1.5rem}article{border:1px solid #7776;border-radius:.4rem;padding:1rem;margin-block:.75rem}h3{overflow-wrap:anywhere}p,dd,li{overflow-wrap:anywhere}.controls{display:flex;gap:.75rem;flex-wrap:wrap;align-items:end}label{display:grid;gap:.3rem}button,select{min-height:2.5rem}dl{display:grid;grid-template-columns:minmax(8rem,1fr) 3fr;gap:.5rem}dd{margin:0}[role=alert]{border-left:3px solid #d98428;padding-left:.75rem}@media(max-width:600px){dl{grid-template-columns:1fr}dd{margin-bottom:.5rem}}
</style>
