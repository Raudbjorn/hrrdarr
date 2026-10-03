import test from 'node:test';
import assert from 'node:assert/strict';
import {describeHostSettings,describeHostError,describeHostResult,sourceLabel,LIMITS_WORDING} from '../src/lib/general-settings.ts';
import {getHostSettings} from '../src/lib/api.ts';
const base=()=>({authentication:'none',configured_bind:'127.0.0.1:8760',bound_address:'127.0.0.1:8760',bind_source:'default',
  allowed_hosts:[],allowed_hosts_source:'default',filtering_enabled:false,trusted_networks:[],trusted_networks_source:'default',forwarding_enabled:false,
  mutability:'deployment',apply_mode:'process_restart',capabilities:{persisted_edits:false,tls:false,url_base:false,trusted_forwarding:true}});
const ready=raw=>{const s=describeHostSettings(raw);assert.equal(s.kind,'ready');return s.view;};
test('default config shows defaults, no auth and filtering/forwarding disabled',()=>{
  const v=ready(base());
  assert.match(v.authentication,/^None/);
  assert.equal(v.bind.configured,'127.0.0.1:8760');assert.match(v.bind.source,/Built-in default/);
  assert.deepEqual(v.allowedHosts.entries,[]);assert.match(v.allowedHosts.source,/Built-in default/);
  assert.match(v.trustedNetworks.summary,/Forwarding disabled/);
  assert.deepEqual(v.limits,[...LIMITS_WORDING]);
  assert.match(v.limits.join(' '),/do not restrict client IP addresses/);
  assert.match(v.limits.join(' '),/restart the process externally/);
});
test('configured lists, sources and bound port 0 are rendered',()=>{
  const raw={...base(),configured_bind:'127.0.0.1:0',bound_address:'127.0.0.1:41235',bind_source:'environment',
    allowed_hosts:['a.test','b.test'],allowed_hosts_source:'environment',filtering_enabled:true,
    trusted_networks:['10.0.0.0/8'],trusted_networks_source:'environment',forwarding_enabled:true};
  const v=ready(raw);
  assert.equal(v.bind.bound,'127.0.0.1:41235');assert.equal(v.bind.source,'Environment variable HRRDARR_BIND');
  assert.deepEqual(v.allowedHosts.entries,['a.test','b.test']);assert.match(v.allowedHosts.summary,/enabled.*2 entries/);
  assert.equal(v.allowedHosts.source,'Environment variable HRRDARR_ALLOWED_HOSTS');
  assert.match(v.trustedNetworks.summary,/1 entry/);assert.equal(v.trustedNetworks.source,'Environment variable HRRDARR_TRUSTED_NETWORKS');
});
test('source labels',()=>{
  assert.match(sourceLabel('default','X'),/Built-in default/);assert.equal(sourceLabel('environment','X'),'Environment variable X');
});
test('explicitly empty environment list means filtering disabled but source environment',()=>{
  const v=ready({...base(),allowed_hosts_source:'environment'});
  assert.match(v.allowedHosts.summary,/Filtering disabled/);assert.match(v.allowedHosts.source,/Environment variable/);assert.deepEqual(v.allowedHosts.entries,[]);
});
test('unimplemented settings are listed as unavailable and no editable claim is made',()=>{
  const v=ready(base());
  for(const k of ['API key','Log level','SSL / TLS','URL base'])assert.ok(v.unavailable.includes(k),k);
  assert.ok(!JSON.stringify(v).match(/can be saved|Save changes/));
});
test('error and malformed responses yield error state',()=>{
  assert.deepEqual(describeHostError('boom'),{kind:'error',message:'boom'});
  assert.match(describeHostError('  ').message,/failed/);
  assert.equal(describeHostResult({ok:false,error:'API request failed (500).'}).kind,'error');
  for(const bad of [null,'x',{},{...base(),authentication:'basic'},{...base(),bind_source:'file'},{...base(),allowed_hosts:[1]},{...base(),capabilities:null},{...base(),mutability:'editable'}])
    assert.equal(describeHostSettings(bad).kind,'error');
});
test('getHostSettings issues a bare GET and surfaces failures as results',async()=>{
  const old=globalThis.fetch,calls=[];
  try{
    globalThis.fetch=async(path,options)=>{calls.push({path,options});return new Response(JSON.stringify({error:{code:'host_not_allowed',message:'Host not allowed.'}}),{status:403});};
    const r=await getHostSettings();
    assert.equal(calls[0].path,'/api/v1/config/host');assert.equal(calls[0].options.method,undefined);assert.equal(calls[0].options.body,undefined);
    assert.equal(r.ok,false);assert.equal(describeHostResult(r).kind,'error');
  }finally{globalThis.fetch=old;}
});
