// Owned library_ui_fixture only. Regression: Activity polling must survive a provider write that was
// in flight when Activity became visible and then failed (so providerVersion never changed).
import assert from 'node:assert/strict';
import {pathToFileURL} from 'node:url';
const {chromium}=await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE||'/usr/lib/node_modules/playwright/index.mjs').href);
const origin=process.env.UI_URL,remote=process.env.UI_PROVIDER_ORIGIN;
for(const value of [origin,remote])assert.equal(new URL(value).hostname,'127.0.0.1');
const browser=await chromium.launch({headless:true,...(process.env.PLAYWRIGHT_EXECUTABLE?{executablePath:process.env.PLAYWRIGHT_EXECUTABLE}:{})});
try {
  const page=await browser.newPage();page.setDefaultTimeout(20000);
  // Keep Fetch interception on for the whole run so route/unroute never drops an app request.
  await page.context().route('**/api/**',route=>route.fallback());
  const errors=[];page.on('pageerror',error=>errors.push(error.message));
  page.on('dialog',dialog=>{errors.push(`Unexpected dialog: ${dialog.message()}`);void dialog.dismiss();});
  const nav=page.getByRole('navigation',{name:'Workspace'}),editor=page.getByRole('article',{name:'Provider editor'});
  const activity=page.getByRole('region',{name:'Activity',exact:true});
  const api=async(method,path,data,status=200)=>{
    const response=await page.request[method](origin+path,{...(data===undefined?{}:{data}),timeout:10000});
    assert.equal(response.status(),status,await response.text());return response.json();
  };
  const scope=category=>({category,imported_category:null,recent_priority:0,older_priority:0});
  const client=await api('post','/api/v1/providers',{name:'Related write client',enabled:true,priority:1,settings:{implementation:'qbittorrent',endpoint:remote,tv:scope('tv'),movies:scope('movies')},credentials:{kind:'username_password',username:'fixture-user',password:'fixture-good'}},201);
  await page.goto(origin);
  // Visit Activity once so it is mounted, then leave it hidden.
  await nav.getByRole('button',{name:'Activity',exact:true}).click();
  const total=async()=>{const text=await activity.getByText(/^\d+ matching commands$/).textContent();return Number(text.split(' ')[0]);};
  await activity.getByText(/^\d+ matching commands$/).waitFor();
  // Start a provider save and hold its PUT so the provider write stays busy.
  await nav.getByRole('button',{name:'Providers',exact:true}).click();
  const list=page.getByRole('complementary',{name:'Saved providers'});
  await list.getByRole('button',{name:'Refresh providers',exact:true}).click();
  await list.getByRole('button',{name:new RegExp(`^${client.name.replace(/[.*+?^${}()|[\]\\]/g,'\\$&')} `)}).click();
  await editor.getByRole('heading',{name:client.name,exact:true}).waitFor();
  const name=editor.getByLabel('Name',{exact:true});
  await name.fill(`${client.name} renamed`);
  const pattern=`**/api/v1/providers/${client.id}`;
  let release;const held=new Promise(resolve=>release=resolve);
  await page.route(pattern,route=>route.request().method()==='PUT'?release(route):route.fallback());
  await editor.getByRole('button',{name:'Save provider',exact:true}).click();
  const heldRoute=await held;
  // While the write is busy, open Activity: read() returns early because of the related write.
  await nav.getByRole('button',{name:'Activity',exact:true}).click();
  await page.waitForTimeout(500);
  const before=await total();
  // The write fails with a conflict: providerVersion does not change.
  await heldRoute.fulfill({status:409,contentType:'application/json',body:JSON.stringify({error:'provider_revision_conflict',message:'Provider changed'})});
  await page.unroute(pattern);
  // A new command created outside the UI must appear through Activity's own polling, no reload.
  const created=await api('post','/api/v1/commands',{name:'refresh_downloads',target:{provider_id:client.id,media_type:'tv'},provider_revision:client.revision,priority:'normal'},202);
  assert.ok(created.id);
  const deadline=Date.now()+15000;let after=before;
  while(Date.now()<deadline){after=await total();if(after>before)break;await page.waitForTimeout(250);}
  assert.ok(after>before,`Activity stopped polling after a failed related write (before ${before}, after ${after})`);
  // Nothing was renamed on the server.
  const stored=await api('get',`/api/v1/providers/${client.id}`);
  assert.equal(stored.name,client.name);
  assert.deepEqual(errors,[]);
  console.log('PASS activity related-write: polling resumes after a failed provider write');
} finally {await browser.close();}
