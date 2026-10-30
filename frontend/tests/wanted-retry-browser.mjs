// Browser-only regression: synthetic API responses exercise retained request identity, not server admission.
// Run against an owned loopback Vite server with UI_URL and optional PLAYWRIGHT_MODULE/PLAYWRIGHT_EXECUTABLE.
import assert from 'node:assert/strict';
import {pathToFileURL} from 'node:url';
const {chromium}=await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE||'/usr/lib/node_modules/playwright/index.mjs').href);
const origin=process.env.UI_URL;assert.equal(new URL(origin).hostname,'127.0.0.1');
const browser=await chromium.launch({headless:true,...(process.env.PLAYWRIGHT_EXECUTABLE?{executablePath:process.env.PLAYWRIGHT_EXECUTABLE}:{})});
const page=await browser.newPage();page.setDefaultTimeout(10000);
const errors=[],sent=[];page.on('pageerror',error=>errors.push(error.message));
const indexer={id:'c0000000-0000-4000-8000-000000000001',revision:1,name:'Both-domain indexer',enabled:true,priority:1,has_credentials:false,
 settings:{implementation:'torznab',endpoint:'http://127.0.0.1',tv:{enable_automatic_search:true},movies:{enable_automatic_search:true}}};
// Deliberately colliding IDs make a changed domain a changed target even when the body is identical.
const episode={target:{media_type:'episode',id:7},series_id:1,series_title:'Harbor',series_type:'standard',series_monitored:true,
 season:1,season_monitored:true,number:1,absolute_episode_number:null,scene_season_number:null,scene_episode_number:null,
 title:'Pilot',monitored:true,effective_monitored:true,air_date_utc:'2020-01-01T00:00:00Z',file:null,cutoff:null};
const movie={target:{media_type:'movie',id:7},title:'Harbor',year:2020,monitored:true,in_cinemas:null,digital_release:null,
 physical_release:null,availability:'available',minimum_availability:'released',file:null,cutoff:null};
try{
 await page.route('**/api/**',async route=>{
  const request=route.request(),path=new URL(request.url()).pathname;
  if(path.endsWith('/wanted/search')){
   sent.push({path,body:request.postDataJSON()});
   return route.fulfill({status:503,json:{error:{code:'unavailable',message:'Admission outcome is uncertain'}}});
  }
  if(path.endsWith('/wanted/missing')){
   const domain=path.includes('/tv/')?'tv':'movies';
   return route.fulfill({json:{media_type:domain,kind:'missing',items:[domain==='tv'?episode:movie],total:1,limit:50,offset:0,
    sort:domain==='tv'?'air_date':'title',direction:'asc',monitored:'monitored',evaluated_at:1,diagnostics:{}}});
  }
  return route.fulfill({json:{items:path==='/api/v1/providers'?[indexer]:[],total:path==='/api/v1/providers'?1:0,limit:25,offset:0}});
 });
 await page.goto(`${origin}/#/wanted/missing`);
 const wanted=page.getByRole('region',{name:'Wanted',exact:true});
 const domains=wanted.getByRole('navigation',{name:'Wanted media type'});
 for(const domain of ['tv','movies']){
  if(domain==='movies')await domains.getByRole('button',{name:'Movies',exact:true}).click();
  await wanted.getByRole('table',{name:domain==='tv'?'Missing episodes':'Missing movies',exact:true}).waitFor();
  await wanted.getByRole('combobox',{name:/^Wanted search indexer/}).selectOption(indexer.id);
  await wanted.getByLabel('Select all on this page',{exact:true}).check();
  const first=sent.length;
  await wanted.getByRole('button',{name:'Search selected (1)',exact:true}).click();
  const retry=wanted.getByRole('button',{name:'Retry same search request',exact:true});await retry.waitFor();
  assert.equal(sent.length,first+1,'An uncertain write is not retried automatically');
  assert.equal(await domains.getByRole('button',{name:'TV',exact:true}).isDisabled(),true);
  assert.equal(await domains.getByRole('button',{name:'Movies',exact:true}).isDisabled(),true);
  assert.equal(sent[first].path,`/api/v1/${domain}/wanted/search`);
  assert.deepEqual(sent[first].body.selection,{kind:'ids',ids:[7]});
  const response=page.waitForResponse(r=>r.request().method()==='POST'&&new URL(r.url()).pathname.endsWith('/wanted/search'));
  await retry.click();await response;
  await wanted.getByRole('status').filter({hasText:'Submitting search request'}).waitFor({state:'hidden'});
  assert.equal(sent.length,first+2);
  assert.deepEqual(sent[first+1],sent[first],'Retry preserves the domain, request ID and entire body');
  await wanted.getByRole('button',{name:'Discard pending request',exact:true}).click();
 }
 assert.deepEqual(errors,[]);
 console.log('PASS Wanted uncertain retries: TV/movie switches blocked, colliding IDs retain their domain, exact request bodies replayed in both domains.');
}catch(error){console.error('PAGESTATE',await page.locator('body').innerText());console.error('ERRORS',errors);throw error;}finally{await browser.close();}
