// Owned library_ui_fixture + in-process fake HDBits and Nyaa servers on 127.0.0.1:0 only; never a live tracker.
// Drives the real ProviderPanel against real provider endpoints for the tracker indexers.
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {mkdir,realpath} from 'node:fs/promises';
import {isAbsolute,join,resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
const {chromium}=await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE||'/usr/lib/node_modules/playwright/index.mjs').href);
const origin=process.env.UI_URL;assert.equal(new URL(origin).hostname,'127.0.0.1');
const fixture=JSON.parse(process.env.UI_FIXTURE_JSON);assert.equal(new URL(fixture.api).hostname,'127.0.0.1');
// Evidence stays in an explicitly selected ignored iteration123 directory, independent of cwd.
assert.ok(process.env.UI_SCREENSHOT_DIR&&isAbsolute(process.env.UI_SCREENSHOT_DIR),'Set an absolute UI_SCREENSHOT_DIR');
const artifactPattern=/\/\.do-not-commit\/loop\/iteration123(?:[-/]|$)/;
const screenshotRequested=resolve(process.env.UI_SCREENSHOT_DIR);assert.match(screenshotRequested,artifactPattern,'Screenshots must stay under ignored iteration123 artifacts');
await mkdir(screenshotRequested,{recursive:true});
const screenshotDirectory=await realpath(screenshotRequested);assert.match(screenshotDirectory,artifactPattern,'Screenshot directory must resolve to ignored iteration123 artifacts');
const screenshotRun=`${Date.now()}-${process.pid}`,screenshots=[];

// ---- Owned fake trackers (HDBits JSON API, Nyaa RSS) on 127.0.0.1:0 only; never a live tracker. ----
const USER='hd-browser-user',PASSKEY='hd-browser-passkey-7c1d',COOKIE='nyaa-cookie-secret-3f9a';
const seen=[];
const server=createServer((request,response)=>{
 const chunks=[];request.on('data',c=>chunks.length<64&&chunks.push(c));
 request.on('end',()=>{
  const url=new URL(request.url,'http://127.0.0.1');let body=null;try{body=JSON.parse(Buffer.concat(chunks).toString('utf8'));}catch{}
  seen.push({method:request.method,path:url.pathname,query:url.search,body_keys:body?Object.keys(body).sort():null,authed:body?.username===USER&&body?.passkey===PASSKEY,cookie:request.headers.cookie??null});
  if(url.pathname==='/hdbits/api/torrents'){
   response.writeHead(200,{'content-type':'application/json'});
   return response.end(JSON.stringify({status:0,data:[{id:101,name:'Browser.Movie.2024.1080p.BluRay.x264-GRP',size:1073741824,utadded:1704110400,seeders:42,leechers:3,times_completed:9,imdb:{id:1234567},type_category:1}]}));
  }
  if(url.pathname==='/nyaa/'){
   response.writeHead(200,{'content-type':'application/rss+xml'});
   return response.end('<?xml version="1.0"?><rss version="2.0" xmlns:nyaa="https://nyaa.si/xmlns/nyaa"><channel><title>Nyaa</title><item><title>[Grp] Browser Anime - 01 [1080p]</title><link>http://127.0.0.1/dl/1.torrent</link><guid isPermaLink="false">n1</guid><pubDate>Mon, 01 Jan 2024 12:00:00 -0000</pubDate><nyaa:seeders>12</nyaa:seeders><nyaa:leechers>1</nyaa:leechers><nyaa:size>700 MiB</nyaa:size></item></channel></rss>');
  }
  response.writeHead(404);response.end('not found');
 });
});
await new Promise((ok,fail)=>{server.once('error',fail);server.listen(0,'127.0.0.1',ok);});
const port=server.address().port,base=`http://127.0.0.1:${port}`;

const browser=await chromium.launch({headless:true,...(process.env.PLAYWRIGHT_EXECUTABLE?{executablePath:process.env.PLAYWRIGHT_EXECUTABLE}:{})});
const page=await browser.newPage({viewport:{width:1280,height:900}});page.setDefaultTimeout(20000);
const errors=[];page.on('pageerror',error=>errors.push(error.message));
page.on('dialog',dialog=>{errors.push(`Unexpected dialog: ${dialog.message()}`);void dialog.dismiss();});
const secrets=[PASSKEY,COOKIE];
const leaks=[];page.on('response',async response=>{if(!new URL(response.url()).pathname.startsWith('/api/'))return;try{const text=await response.text();if(secrets.some(s=>text.includes(s)))leaks.push(new URL(response.url()).pathname);}catch{}});
const nav=page.getByRole('navigation',{name:'Workspace'}),editor=page.getByRole('article',{name:'Provider editor'});
async function screenshot(name){const path=join(screenshotDirectory,`iteration123-tracker-${name}-${screenshotRun}.png`);await page.screenshot({path,fullPage:true});screenshots.push(path);}
async function api(method,path,data,status=200){const r=await page.request[method](origin+path,{...(data===undefined?{}:{data}),timeout:40000});const text=await r.text();assert.equal(r.status(),status,text);assert.ok(!secrets.some(s=>text.includes(s)),`Secret echoed by ${path}`);return status===204?null:JSON.parse(text);}
async function until(read,accept,label,timeoutMs=30000){const end=Date.now()+timeoutMs;let last;while(Date.now()<end){last=await read();if(accept(last))return last;await page.waitForTimeout(100);}assert.fail(`${label}: ${JSON.stringify(last)}`);}
async function ready(locator){await locator.waitFor();await until(()=>locator.isEnabled(),Boolean,'Control remained disabled');}
const SELECTS=new Set(['Provider type','Credential action']);
const field=label=>SELECTS.has(label)?editor.getByLabel(new RegExp(`^${label}`)):editor.getByLabel(label,{exact:true});
async function newProvider(type,name,endpoint){
 await nav.getByRole('button',{name:'Providers',exact:true}).click();
 await ready(page.getByRole('button',{name:'New provider',exact:true}));await page.getByRole('button',{name:'New provider',exact:true}).click();
 await field('Provider type').selectOption(type);await field('Name').fill(name);await field('Endpoint').fill(endpoint);
}
async function save(){
 const button=editor.getByRole('button',{name:'Save provider',exact:true});await ready(button);
 const saved=page.waitForResponse(r=>r.request().method()==='POST'&&new URL(r.url()).pathname==='/api/v1/providers');
 await button.click();const response=await saved;assert.equal(response.status(),201,await response.text());
 await ready(field('Name'));return response.json();
}
async function testSaved(provider){
 const button=editor.getByRole('button',{name:'Test saved provider',exact:true});
 for(let attempt=0;attempt<40;attempt++){
  await ready(button);
  const reply=page.waitForResponse(r=>new URL(r.url()).pathname===`/api/v1/providers/${provider.id}/test`&&r.request().method()==='POST');
  await button.click();const response=await reply;const body=await response.json();
  if(response.status()===429&&body.error?.code==='provider_busy'){await page.waitForTimeout(250);continue;}
  return {status:response.status(),body};
 }
 assert.fail('Saved Test stayed busy');
}

try{
 await page.goto(origin);
 // 1. HDBits: account credentials, movie + TV scopes, category ids and minimum seeders.
 await newProvider('hdbits','HDBits browser',`${base}/hdbits/api/torrents`);
 assert.equal(await field('TV interactive search').isChecked(),true,'Searchable trackers default search flags on');
 await field('Movie scope').check();await field('Movie minimum seeders').fill('5');await field('Movie category ids').fill('1');
 await field('Credential action').selectOption('replace');await field('Username').fill(USER);await field('Passkey').fill(PASSKEY);
 await screenshot('hdbits-draft');
 const hd=await save();
 assert.equal(hd.settings.implementation,'hdbits');assert.equal(hd.has_credentials,true);
 const stored=await api('get',`/api/v1/providers/${hd.id}`);
 assert.equal(stored.settings.movies.minimum_seeders,5);assert.deepEqual(stored.settings.movies.categories,[1]);assert.equal(stored.settings.tv.minimum_seeders??null,null);
 assert.equal(await editor.getByLabel('Passkey',{exact:true}).count(),0,'Passkey must not be prefilled after save');
 const tested=await testSaved(hd);
 assert.equal(tested.status,200,JSON.stringify(tested.body));
 assert.ok(seen.some(r=>r.path==='/hdbits/api/torrents'&&r.method==='POST'&&r.authed),'HDBits request carried the sealed credentials in its JSON body');
 assert.ok(seen.filter(r=>r.path==='/hdbits/api/torrents').every(r=>r.query===''),'No credentials in the URL');
 await screenshot('hdbits-tested');
 // 2. Nyaa: feed credential bundle (cookie), RSS-only draft scope without categories.
 await newProvider('nyaa','Nyaa browser',`${base}/nyaa/`);
 assert.equal(await editor.getByLabel('Movie category ids',{exact:true}).count(),0,'Nyaa has no category selector');
 await field('Credential action').selectOption('replace');await field('Cookie (optional)').fill(COOKIE);
 const nyaa=await save();assert.equal(nyaa.settings.implementation,'nyaa');
 const nyaaTest=await testSaved(nyaa);
 assert.equal(nyaaTest.status,200,JSON.stringify(nyaaTest.body));
 assert.ok(seen.some(r=>r.path==='/nyaa/'&&r.cookie===COOKIE),'Feed cookie was sent from the sealed bundle');
 await screenshot('nyaa-tested');
 // 3. Both are listed as indexers and no secret ever came back.
 const list=await api('get','/api/v1/providers');
 assert.ok(['hdbits','nyaa'].every(i=>list.items.some(p=>p.settings.implementation===i)));
 assert.deepEqual(leaks,[],'Secret leaked into an API response');assert.deepEqual(errors,[]);
 console.log(JSON.stringify({screenshots,requests:seen.length}));
 console.log('PASS Tracker ProviderPanel: HDBits (account credentials in the JSON body, minimum seeders, categories) and Nyaa (feed cookie) created, tested and listed through the real editor; no secret in any API response or URL.');
}catch(error){console.error('PAGESTATE',(await page.locator('body').innerText().catch(()=>'')).slice(0,8000));console.error('SEEN',JSON.stringify(seen.slice(-20)));throw error;}
finally{try{await browser.close();}finally{const closed=new Promise(done=>server.close(()=>done()));server.closeAllConnections();await closed;}}
