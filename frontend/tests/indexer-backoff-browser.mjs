// Owned loopback HTTP failure -> persisted backoff -> Health UI -> saved-Test recovery. No overlays.
import assert from 'node:assert/strict';
import {pathToFileURL} from 'node:url';
const {chromium}=await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE||'/usr/lib/node_modules/playwright/index.mjs').href);
const origin=process.env.UI_URL,remote=process.env.UI_PROVIDER_ORIGIN;
for(const value of [origin,remote])assert.equal(new URL(value).hostname,'127.0.0.1');
const browser=await chromium.launch({headless:true,...(process.env.PLAYWRIGHT_EXECUTABLE?{executablePath:process.env.PLAYWRIGHT_EXECUTABLE}:{})});
const page=await browser.newPage();page.setDefaultTimeout(20000);
const errors=[];page.on('pageerror',error=>errors.push(error.message));
page.on('dialog',dialog=>{errors.push(`Unexpected dialog: ${dialog.message()}`);void dialog.dismiss();});
const nav=page.getByRole('navigation',{name:'Workspace'}),editor=page.getByRole('article',{name:'Provider editor'}),health=page.getByRole('region',{name:'Health',exact:true});
async function get(path){const response=await page.request.get(origin+path,{timeout:10000});assert.equal(response.status(),200,await response.text());return response.json();}
async function until(read,accept,label){const end=Date.now()+25000;while(Date.now()<end){const value=await read();if(accept(value))return value;await page.waitForTimeout(100);}assert.fail(label);}
async function mode(value){assert.equal((await page.request.post(`${remote}/fixture-indexer-mode?mode=${value}`,{timeout:10000})).status(),204);}
async function idle(){await until(()=>health.getAttribute('aria-busy'),value=>value==='false','Health read did not finish');}
async function edit(provider){
 await nav.getByRole('button',{name:'Providers',exact:true}).click();
 const list=page.getByRole('complementary',{name:'Saved providers'});
 await list.getByRole('button',{name:'Refresh providers',exact:true}).click();
 await list.getByRole('button',{name:new RegExp(`^${provider.name} `)}).click();
 await editor.getByRole('heading',{name:provider.name,exact:true}).waitFor();
}
async function testSaved(provider,success){
 await edit(provider);
 const response=page.waitForResponse(r=>new URL(r.url()).pathname===`/api/v1/providers/${provider.id}/test`&&r.request().method()==='POST');
 await editor.getByRole('button',{name:'Test saved provider',exact:true}).click();
 const reply=await response;assert.equal(reply.status(),success?200:429,await reply.text());
 const saved=await until(()=>get(`/api/v1/providers/${provider.id}`),p=>p.last_test?.status===(success?'success':'failure'),'Saved test observation missing');
 assert.equal(saved.revision,provider.revision);
 assert.equal(saved.last_test.revision,provider.revision);
 await until(()=>editor.getByRole('button',{name:'Test saved provider',exact:true}).isEnabled(),Boolean,'Saved Test readback incomplete');
}
async function runHealth(domain){
 await nav.getByRole('button',{name:'Health',exact:true}).click();await idle();
 await health.getByLabel('Health scope').selectOption(domain);await idle();
 await until(()=>get('/api/v1/health'),s=>!s.active_command,'Prior health work did not settle');
 await health.getByRole('button',{name:'Refresh health',exact:true}).click();await idle();
 const response=page.waitForResponse(r=>new URL(r.url()).pathname==='/api/v1/health/commands'&&r.request().method()==='POST');
 await health.getByRole('button',{name:`Run ${domain} health checks`,exact:true}).click();
 const reply=await response;assert.equal(reply.status(),202);const admission=await reply.json();
 assert.equal(admission.selection.length,9); // Schema0050 adds two backoff identities to seven existing checks.
 const snapshot=await until(()=>get(`/api/v1/health?scope=${domain}`),s=>s.summary.current===9&&!s.active_command&&admission.selection.every(token=>s.checks.some(check=>check.identity.check_key===token.identity.check_key&&check.observed_generation>=token.generation)),'Requested health observations did not settle');
 await health.getByRole('button',{name:'Refresh health',exact:true}).click();await idle();return snapshot;
}
try{
 await mode(0);
 const created=await page.request.post(origin+'/api/v1/providers',{data:{name:'Backoff browser indexer',enabled:true,priority:1,settings:{implementation:'torznab',endpoint:`${remote}/torznab`,tv:{categories:[5030],anime_categories:[5070]},movies:{categories:[2000]}},credentials:{kind:'api_key',api_key:'fixture-good'}},timeout:10000});
 assert.equal(created.status(),201,await created.text());const provider=await created.json();
 await page.goto(origin);await testSaved(provider,true);
 await mode(1);await testSaved(provider,false);
 for(const domain of ['tv','movies']){
  const snapshot=await runHealth(domain),issue=snapshot.issues.find(i=>i.identity.check_key==='indexer_status');
  assert.ok(issue);assert.equal(issue.severity,'error');assert.equal(issue.reason,'indexers_temporary_backoff');assert.equal(issue.compatibility_type,'IndexerStatusCheck');
  assert.equal(snapshot.issues.some(i=>i.identity.check_key==='indexer_failures'),false);
  assert.equal(JSON.stringify(snapshot).includes('PRIVATE_FIXTURE_INDEXER_FAILURE'),false);assert.equal(JSON.stringify(snapshot).includes(remote),false);
  const row=health.getByRole('article',{name:`${domain}:indexer_status`,exact:true});await row.getByText(`error: ${issue.message} (${issue.reason})`,{exact:true}).waitFor();
  assert.equal(await row.getByRole('link',{name:'Check help'}).getAttribute('href'),`https://wiki.servarr.com/${domain==='tv'?'sonarr':'radarr'}/system#indexers-are-unavailable-due-to-failures`);
  await health.getByRole('region',{name:'Health diagnostic transitions'}).getByText(new RegExp(`issue — ${domain}:indexer_status`)).first().waitFor();
 }
 // Saved Test bypasses the still-active ten-minute fixture Retry-After to restore both scopes immediately.
 await mode(0);await testSaved(provider,true);
 for(const domain of ['tv','movies']){
  const snapshot=await runHealth(domain);
  assert.equal(snapshot.issues.some(i=>['indexer_status','indexer_failures'].includes(i.identity.check_key)),false);
  const row=health.getByRole('article',{name:`${domain}:indexer_status`,exact:true});await row.getByText('No cached issue on this check; evaluation state determines freshness.',{exact:true}).waitFor();
  await health.getByRole('region',{name:'Health diagnostic transitions'}).getByText(new RegExp(`restored — ${domain}:indexer_status`)).first().waitFor();
 }
 // All eighteen tokens must be observed even though the first visible page holds only sixteen checks.
 await health.getByLabel('Health scope').selectOption('all');await idle();
 const admitted=page.waitForResponse(r=>new URL(r.url()).pathname==='/api/v1/health/commands'&&r.request().method()==='POST');
 await health.getByRole('button',{name:'Run all health checks',exact:true}).click();
 const admissionResponse=await admitted;assert.equal(admissionResponse.status(),202);assert.equal((await admissionResponse.json()).selection.length,18);
 const allRequest=health.getByRole('region',{name:'Manual health requests'}).getByRole('article').filter({has:page.getByRole('heading',{name:/Request \d+: all —/})});
 await allRequest.getByText('Requested generations observed in the current epoch.',{exact:true}).waitFor();
 assert.equal(await health.getByRole('region',{name:'Registered check coverage'}).getByRole('article').count(),16);
 assert.deepEqual(errors,[]);
 console.log('PASS real saved-Test HTTP429 produces both-domain transient backoff and Health issue; no persistent misclassification/private response leakage; successful saved Test restores both scopes before Retry-After expiry; Health issue/restored transitions visible; all18 request tokens observed while16 rows are displayed.');
}catch(error){console.error('PAGESTATE',(await page.locator('body').innerText()).slice(0,10000));throw error;}
finally{try{await mode(0);}finally{await browser.close();}}
