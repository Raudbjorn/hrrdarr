// Real owned metadata404 -> persisted lifecycle -> Health API/UI -> restoration. No overlays.
import assert from 'node:assert/strict';
import {pathToFileURL} from 'node:url';
const {chromium}=await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE||'/usr/lib/node_modules/playwright/index.mjs').href);
const origin=process.env.UI_URL,metadata=process.env.UI_METADATA_ORIGIN;
for(const value of [origin,metadata])assert.equal(new URL(value).hostname,'127.0.0.1');
assert.ok(process.env.UI_TV_ROOT&&process.env.UI_MOVIE_ROOT,'Owned library roots required');
const browser=await chromium.launch({headless:true,...(process.env.PLAYWRIGHT_EXECUTABLE?{executablePath:process.env.PLAYWRIGHT_EXECUTABLE}:{})});
const page=await browser.newPage();page.setDefaultTimeout(20000);
const errors=[],writes=[];page.on('pageerror',error=>errors.push(error.message));page.on('dialog',dialog=>{errors.push('Unexpected dialog: '+dialog.message());void dialog.dismiss();});
page.on('request',request=>{if(!['GET','HEAD'].includes(request.method()))writes.push([request.method(),new URL(request.url()).pathname]);});
const get=async path=>{const r=await page.request.get(origin+path,{timeout:10000});assert.equal(r.status(),200,await r.text());return r.json();};
const until=async(fn,accept,label)=>{const deadline=Date.now()+25000;while(Date.now()<deadline){const value=await fn();if(accept(value))return value;await page.waitForTimeout(100);}assert.fail(label);};
const mode=async n=>assert.equal((await page.request.post(`${metadata}/fixture-metadata-mode?mode=${n}`,{timeout:10000})).status(),204);
const health=page.getByRole('region',{name:'Health',exact:true});
const idle=()=>until(()=>health.getAttribute('aria-busy'),v=>v==='false','Health readback incomplete');
const refreshHealth=async()=>{await idle();await health.getByRole('button',{name:'Refresh health',exact:true}).click();await idle();};
const collection=domain=>domain==='tv'?'/api/v1/tv/series':'/api/v1/movies';
const removed=s=>s.issues.find(i=>i.identity.check_key==='removed_metadata');
async function add(domain){
 const title=domain==='tv'?'Fixture series':'Fixture movie';
 await page.getByRole('button',{name:domain==='tv'?'TV':'Movies',exact:true}).click();
 await page.getByRole('button',{name:domain==='tv'?'Add series':'Add movie',exact:true}).click();
 await page.getByLabel('Search catalogue').fill('Fixture');await page.getByRole('button',{name:'Search',exact:true}).click();
 await page.getByRole('button',{name:new RegExp(title+' .*')}).click();
 await page.getByLabel('Existing library directory').fill(domain==='tv'?process.env.UI_TV_ROOT:process.env.UI_MOVIE_ROOT);
 await page.getByRole('button',{name:'Add to library',exact:true}).click();
 await page.getByRole('article').getByRole('heading',{name:title,exact:true}).waitFor();
 const list=await get(collection(domain));
 const item=list.items.find(i=>(domain==='tv'?i.tvdb_id:i.tmdb_id)===101);assert.ok(item);return item;
}
async function metadataRefresh(domain,id,status){
 const target=domain==='tv'?{media_type:domain,series_id:id}:{media_type:domain,movie_id:id};
 const response=await page.request.post(origin+'/api/v1/metadata-refresh/commands',{data:{target,priority:'normal'},timeout:10000});assert.equal(response.status(),202,await response.text());
 const accepted=await response.json();const command=await until(()=>get('/api/v1/metadata-refresh/commands/'+accepted.id),v=>['succeeded','failed','cancelled'].includes(v.status),'Metadata command did not settle');
 assert.equal(command.status,status);assert.equal(command.external_id,101);if(status==='failed')assert.equal(command.error_code,'metadata_not_found');return command;
}
async function run(domain){
 await health.getByLabel('Health scope').selectOption(domain);await idle();
 await until(()=>get('/api/v1/health'),s=>!s.active_command,'Earlier health work did not settle');await refreshHealth();
 const button=health.getByRole('button',{name:`Run ${domain} health checks`,exact:true});
 const reply=page.waitForResponse(r=>new URL(r.url()).pathname==='/api/v1/health/commands'&&r.request().method()==='POST');
 await button.focus();await page.keyboard.press('Enter');const response=await reply;assert.equal(response.status(),202);
 const admission=await response.json();assert.equal(admission.requested_scope,domain);assert.equal(admission.selection.length,4);
 // A scheduled batch may win admission; verify every requested generation rather than assuming a new command ID.
 const snapshot=await until(()=>get(`/api/v1/health?scope=${domain}`),s=>s.summary.current===4&&!s.active_command&&admission.selection.every(token=>s.checks.some(check=>check.identity.scope===token.identity.scope&&check.identity.check_key===token.identity.check_key&&check.observed_generation>=token.generation)),'Scoped requested generations not current');await refreshHealth();return snapshot;
}
try{
 await mode(0);await page.goto(origin);const owners={tv:await add('tv'),movies:await add('movies')};
 // Valid response establishes a harmless literal markup title before actual provider disappearance.
 await mode(5);for(const domain of ['tv','movies'])await metadataRefresh(domain,owners[domain].id,'succeeded');
 await page.getByRole('navigation',{name:'Workspace'}).getByRole('button',{name:'Health',exact:true}).click();await idle();
 for(const domain of ['tv','movies'])assert.equal(removed(await run(domain)),undefined);
 await mode(4);for(const domain of ['tv','movies'])await metadataRefresh(domain,owners[domain].id,'failed');
 for(const domain of ['tv','movies']){
  const snapshot=await run(domain),issue=removed(snapshot);assert.ok(issue);assert.equal(issue.severity,'error');assert.equal(issue.reason,domain==='tv'?'removed_series_single':'removed_movie_single');
  assert.ok(issue.message.includes('Removed <img src=x onerror=alert(1)> '+(domain==='tv'?'series':'movie')));assert.ok(issue.message.includes(`${domain==='tv'?'TVDB':'TMDb'} ID 101`));
  const row=health.getByRole('article',{name:`${domain}:removed_metadata`,exact:true});await row.getByText(`error: ${issue.message} (${issue.reason})`,{exact:true}).waitFor();assert.equal(await row.locator('img,script').count(),0);
  await health.getByRole('region',{name:'Health diagnostic transitions'}).getByText(new RegExp(`issue — ${domain}:removed_metadata`)).first().waitFor();
 }
 await mode(5);for(const domain of ['tv','movies'])await metadataRefresh(domain,owners[domain].id,'succeeded');
 for(const domain of ['tv','movies']){
  assert.equal(removed(await run(domain)),undefined);
  await health.getByRole('region',{name:'Health diagnostic transitions'}).getByText(new RegExp(`restored — ${domain}:removed_metadata`)).first().waitFor();
  const item=await get(`${collection(domain)}/${owners[domain].id}`);assert.equal(item.id,owners[domain].id);assert.equal(item.path,owners[domain].path);assert.equal(item.monitored,owners[domain].monitored);
 }
 assert.deepEqual(errors,[]);
 // Every browser mutation is explicit add or keyboard health admission. API fixture/refresh calls above are explicit too.
 assert.ok(writes.length>=8);for(const [method,path] of writes){assert.equal(method,'POST');assert.ok(['/api/v1/tv/series/lookup','/api/v1/movies/lookup','/api/v1/health/commands'].includes(path),`Unexpected mutation ${method} ${path}`);}
 console.log('PASS real both-domain add/404/Error/restoration; source identities, escaped titles, keyboard scoped checks, issue/restored UI, preserved library settings; no overlays or destructive writes');
}finally{try{await mode(0);}finally{await browser.close();}}
