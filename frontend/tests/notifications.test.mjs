import test from 'node:test';
import assert from 'node:assert/strict';
import {blankDraft,buildInput,describeTest,draftFromNotification,TV_TRIGGERS,MOVIE_TRIGGERS} from '../src/lib/notification-draft.ts';
import {listNotifications,createNotification,updateNotification,deleteNotification,testNotificationDraft,testSavedNotification,listNotificationDeliveries} from '../src/lib/api.ts';
import {parseHash,hashFor,SETTINGS_SECTIONS} from '../src/lib/routes.ts';

const field=(over)=>({name:'x',label:'X',kind:'text',required:false,secret:false,help:'',default:null,options:[],min:null,max:null,max_length:512,...over});
const gotify={implementation:'gotify',label:'Gotify',description:'',kind:'message',domains:['tv','movies'],fields:[
  field({name:'server_url',label:'Server URL',kind:'url',required:true}),
  field({name:'app_token',label:'Application token',secret:true,required:true}),
  field({name:'priority',label:'Priority',kind:'number',min:0,max:10,default:5}),
]};
const pushsafer={implementation:'pushsafer',label:'Pushsafer',description:'',kind:'message',domains:['movies'],fields:[field({name:'private_key',label:'Key',secret:true,required:true})]};
async function withFetch(stub,run){const original=globalThis.fetch;globalThis.fetch=stub;try{await run();}finally{globalThis.fetch=original;}}
const json=(value,status=200)=>new Response(JSON.stringify(value),{status});

test('trigger tables are domain specific',()=>{
  const tv=TV_TRIGGERS.map(t=>t.key),movie=MOVIE_TRIGGERS.map(t=>t.key);
  assert.ok(tv.includes('on_series_add')&&!tv.includes('on_movie_added'));
  assert.ok(movie.includes('on_movie_added')&&!movie.includes('on_series_add'));
  assert.ok(tv.includes('on_import_complete')&&!movie.includes('on_import_complete'));
});

test('blank drafts start in a supported domain with defaults',()=>{
  const d=blankDraft(gotify);
  assert.equal(d.tv.on,true);assert.equal(d.movies.on,false);assert.equal(d.settings.priority,5);
  const m=blankDraft(pushsafer);assert.equal(m.tv.on,false);assert.equal(m.movies.on,true);
});

test('buildInput validates like the server and keeps secrets write-only',()=>{
  const d=blankDraft(gotify);
  assert.equal(buildInput(d,gotify).ok,false,'required url and secret');
  d.settings.server_url='http://gotify.local';
  assert.match(buildInput(d,gotify).error,/Application token is required/);
  d.secrets.app_token='tok';
  const ok=buildInput(d,gotify);
  assert.equal(ok.ok,true);
  assert.deepEqual(ok.data.secrets,{app_token:'tok'});
  assert.equal(ok.data.settings.app_token,undefined);
  assert.equal(ok.data.movies,undefined);assert.deepEqual(ok.data.tv.tag_ids,[]);
  // edit: blank keeps, stored satisfies required, clearing a required secret fails
  const e=blankDraft(gotify);e.settings.server_url='https://g.example';e.revision=3;
  const kept=buildInput(e,gotify,['app_token']);assert.equal(kept.ok,true);assert.deepEqual(kept.data.secrets,{});assert.equal(kept.data.revision,3);
  e.cleared=['app_token'];assert.equal(buildInput(e,gotify,['app_token']).ok,false);
  for(const bad of [{priority:11},{priority:1.5},{server_url:'ftp://x'},{server_url:' http://x'}]){
    const c=blankDraft(gotify);c.secrets.app_token='t';c.settings={server_url:'http://g',...bad};
    assert.equal(buildInput(c,gotify).ok,false,JSON.stringify(bad));
  }
  const noscope=blankDraft(gotify);noscope.tv.on=false;noscope.settings.server_url='http://g';noscope.secrets.app_token='t';
  assert.match(buildInput(noscope,gotify).error,/at least one/);
  const upgrade=blankDraft(gotify);upgrade.settings.server_url='http://g';upgrade.secrets.app_token='t';upgrade.tv.triggers.on_upgrade=true;
  assert.match(buildInput(upgrade,gotify).error,/requires its base trigger/);
  upgrade.tv.triggers.on_import=true;assert.equal(buildInput(upgrade,gotify).ok,true);
  const wrong=blankDraft(pushsafer);wrong.tv.on=true;wrong.secrets.private_key='k';
  assert.match(buildInput(wrong,pushsafer).error,/does not support the tv domain/);
  const tags=blankDraft(gotify);tags.settings.server_url='http://g';tags.secrets.app_token='t';tags.tv.tagIds=[1,1];
  assert.match(buildInput(tags,gotify).error,/distinct/);
});

test('a cleared optional number is omitted, not sent as zero',()=>{
  const d=blankDraft(gotify);d.settings.server_url='http://gotify.local';d.secrets.app_token='tok';
  // Svelte's bind:value yields null for an emptied type=number input.
  d.settings.priority=null;
  const ok=buildInput(d,gotify);
  assert.equal(ok.ok,true);
  assert.equal('priority' in ok.data.settings,false);
  const topic=field({name:'topic_id',label:'Topic',kind:'number',min:1,max:100});
  const telegram={...gotify,fields:[topic]};
  const t=blankDraft(telegram);t.settings.topic_id=null;
  assert.equal(buildInput(t,telegram).ok,true,'min 1 must not reject a cleared field');
  t.settings.topic_id=0;
  assert.match(buildInput(t,telegram).error,/whole number from 1/);
});

test('secret URL fields are checked as URLs before save',()=>{
  const discord={implementation:'discord',label:'Discord',description:'',kind:'message',domains:['tv','movies'],fields:[
    field({name:'webhook_url',label:'Webhook URL',kind:'url',secret:true,required:true,max_length:2048}),
  ]};
  const d=blankDraft(discord);d.secrets.webhook_url='not a url';
  assert.match(buildInput(d,discord).error,/Webhook URL must be an http\(s\) URL/);
  d.secrets.webhook_url='https://discord.com/api/webhooks/1/x';
  const ok=buildInput(d,discord);
  assert.equal(ok.ok,true);
  assert.deepEqual(ok.data.secrets,{webhook_url:'https://discord.com/api/webhooks/1/x'});
});

test('drafts from saved notifications never receive secret values',()=>{
  const n={id:'11111111-1111-4111-8111-111111111111',name:'Phone',implementation:'gotify',enabled:true,revision:2,settings:{server_url:'http://g',priority:5},secrets_configured:['app_token'],tv:{triggers:{on_grab:true},tag_ids:[4]},movies:null,created_at:1,updated_at:1};
  const d=draftFromNotification(n,gotify);
  assert.deepEqual(d.secrets,{});assert.equal(d.tv.triggers.on_grab,true);assert.equal(d.tv.triggers.on_import,false);assert.deepEqual(d.tv.tagIds,[4]);assert.equal(d.movies.on,false);
});

test('test outcomes are summarised without remote content',()=>{
  assert.equal(describeTest({id:null,name:'x',results:[{media_type:'tv',status:'delivered',error:null},{media_type:'movies',status:'failed',error:'authentication'}]}),'TV: delivered; Movies: failed (authentication)');
});

test('client stubs use the documented routes and validate ids',async()=>{
  const id='11111111-1111-4111-8111-111111111111',calls=[];
  await withFetch(async(url,init)=>{calls.push([String(url),init?.method??'GET']);return init?.method==='DELETE'?new Response(null,{status:204}):json({items:[]});},async()=>{
    await listNotifications();await createNotification({name:'a'});await updateNotification(id,{name:'a'});await deleteNotification(id);
    await testNotificationDraft({draft:{name:'a'}});await testSavedNotification(id);await listNotificationDeliveries(5);
  });
  assert.deepEqual(calls,[
    ['/api/v1/notifications','GET'],['/api/v1/notifications','POST'],[`/api/v1/notifications/${id}`,'PUT'],[`/api/v1/notifications/${id}`,'DELETE'],
    ['/api/v1/notifications/test','POST'],[`/api/v1/notifications/${id}/test`,'POST'],['/api/v1/notifications/deliveries?limit=5','GET']]);
  assert.equal((await updateNotification('nope',{})).ok,false);
  assert.equal((await deleteNotification('../x')).ok,false);
  assert.equal((await listNotificationDeliveries(0)).ok,false);
});

test('the connect route is listed under settings',()=>{
  assert.deepEqual(parseHash(hashFor('notifications')),{kind:'view',view:'notifications'});
  assert.ok(SETTINGS_SECTIONS.some(s=>s.view==='notifications'));
});
