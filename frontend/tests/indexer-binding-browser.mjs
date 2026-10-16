// Opt-in regression against an owned library_ui_fixture and Vite proxy; no live services.
import assert from 'node:assert/strict';
import {pathToFileURL} from 'node:url';
const {chromium}=await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE||'/usr/lib/node_modules/playwright/index.mjs').href);
const origin=process.env.UI_URL,providerOrigin=process.env.UI_PROVIDER_ORIGIN;
for(const value of [origin,providerOrigin])assert.equal(new URL(value).hostname,'127.0.0.1');
const roots={tv:process.env.UI_TV_ROOT||(process.env.UI_SCRATCH&&`${process.env.UI_SCRATCH}/tv`),movies:process.env.UI_MOVIE_ROOT||(process.env.UI_SCRATCH&&`${process.env.UI_SCRATCH}/movies`)};
assert.ok(roots.tv&&roots.movies,'Set UI_SCRATCH or UI_TV_ROOT/UI_MOVIE_ROOT from the owned fixture');
const browser=await chromium.launch({headless:true,...(process.env.PLAYWRIGHT_EXECUTABLE?{executablePath:process.env.PLAYWRIGHT_EXECUTABLE}:{})});
const page=await browser.newPage();page.setDefaultTimeout(15000);
const errors=[];page.on('pageerror',error=>errors.push(error.message));
page.on('dialog',dialog=>{errors.push(`Unexpected dialog: ${dialog.message()}`);void dialog.dismiss();});
const nav=page.getByRole('navigation',{name:'Workspace'});
const editor=page.getByRole('article',{name:'Provider editor'});
const rss=page.getByRole('region',{name:'RSS automation',exact:true});
const search=page.getByRole('region',{name:'Search and download',exact:true});
async function api(method,path,data,status=200){
 const response=await page.request[method](origin+path,{...(data===undefined?{}:{data}),timeout:10000});
 assert.equal(response.status(),status,await response.text());return status===204?null:response.json();
}
const readProvider=id=>api('get',`/api/v1/providers/${id}`);
async function updateProvider(id,changes){const p=await readProvider(id);return api('put',`/api/v1/providers/${id}`,{revision:p.revision,name:p.name,enabled:p.enabled,priority:p.priority,settings:p.settings,...changes});}
async function ready(locator){await locator.waitFor();await locator.evaluate(el=>new Promise((resolve,reject)=>{const deadline=Date.now()+10000;function check(){if(!el.matches(':disabled'))return resolve();if(Date.now()>deadline)return reject(new Error('Control remained disabled'));setTimeout(check,25);}check();}));}
async function edit(provider){
 await nav.getByRole('button',{name:'Providers',exact:true}).click();
 await page.getByRole('complementary',{name:'Saved providers'}).getByRole('button',{name:'Refresh providers',exact:true}).click();
 await page.getByRole('complementary',{name:'Saved providers'}).getByRole('button',{name:new RegExp(`^${provider.name} `)}).click();
 await editor.getByRole('heading',{name:provider.name,exact:true}).waitFor();
 await ready(editor.getByLabel('Name',{exact:true}));
}
async function save(){
 const response=page.waitForResponse(r=>r.request().method()==='PUT'&&new URL(r.url()).pathname.startsWith('/api/v1/providers/'));
 await editor.getByRole('button',{name:'Save provider',exact:true}).click();assert.equal((await response).status(),200);
 await ready(editor.getByLabel('Name',{exact:true}));
}
async function add(domain){
 await nav.getByRole('button',{name:'Library',exact:true}).click();
 await page.getByRole('navigation',{name:'Library type'}).getByRole('button',{name:domain==='tv'?'TV':'Movies',exact:true}).click();
 await page.getByRole('button',{name:domain==='tv'?'Add series':'Add movie',exact:true}).click();
 await page.getByLabel('Search catalogue',{exact:true}).fill('Fixture');await page.getByRole('button',{name:'Search',exact:true}).click();
 const title=domain==='tv'?'Fixture series':'Fixture movie';
 await page.getByRole('button',{name:new RegExp(`^${title} `)}).click();
 await page.getByLabel('Existing library directory',{exact:true}).fill(roots[domain]);
 await page.getByRole('button',{name:'Add to library',exact:true}).click();
 await page.getByRole('article',{name:'Library detail'}).getByRole('heading',{name:title,exact:true}).waitFor();
}
async function openSearch(domain){
 await nav.getByRole('button',{name:'Library',exact:true}).click();
 await page.getByRole('navigation',{name:'Library type'}).getByRole('button',{name:domain==='tv'?'TV':'Movies',exact:true}).click();
 await page.getByRole('complementary',{name:'Library collection'}).getByRole('button',{name:new RegExp(`^Fixture ${domain==='tv'?'series':'movie'}`)}).click();
 await page.getByRole('button',{name:domain==='tv'?'Search releases for Pilot':'Search movie releases',exact:true}).click();
 await ready(search.getByLabel(/^Download search indexer/));
}
async function openRss(domain){
 await nav.getByRole('button',{name:'RSS',exact:true}).click();
 await ready(rss.getByLabel(/^RSS media/));await rss.getByLabel(/^RSS media/).selectOption(domain);
 await ready(rss.getByLabel(/^RSS indexer/));
}
async function checkSelection(panel,indexerLabel,clientLabel,bound,unbound,client,available){
 const indexer=panel.getByLabel(new RegExp(`^${indexerLabel}`)),select=panel.getByLabel(new RegExp(`^${clientLabel}`));
 await indexer.selectOption(bound.id);
 if(available){await page.waitForFunction(({label,id})=>Array.from(document.querySelectorAll('label')).find(el=>el.textContent.startsWith(label))?.querySelector('select')?.value===id,{label:clientLabel,id:client.id});assert.equal(await select.inputValue(),client.id);}
 else await panel.getByRole('alert').filter({hasText:/bound|configured|client/i}).first().waitFor();
 assert.equal(await select.isDisabled(),true);
 const actions=panel===rss?['Run RSS now','Save RSS schedule']:['Search to choose a download','Search and download best match'];
 for(const name of actions){const button=panel.getByRole('button',{name,exact:true});if(available)await ready(button);else assert.equal(await button.isDisabled(),true,`${name} must fail closed`);}
 await indexer.selectOption(unbound.id);await ready(select);await select.selectOption(client.alternativeId);
 assert.equal(await select.inputValue(),client.alternativeId);
 await indexer.selectOption(bound.id);
 if(available)await page.waitForFunction(({label,id})=>Array.from(document.querySelectorAll('label')).find(el=>el.textContent.startsWith(label))?.querySelector('select')?.value===id,{label:clientLabel,id:client.id});
 else await panel.getByRole('alert').filter({hasText:/bound|configured|client/i}).first().waitFor();
 assert.equal(await select.isDisabled(),true);
 for(const name of actions){const button=panel.getByRole('button',{name,exact:true});if(available)await ready(button);else assert.equal(await button.isDisabled(),true);}
}
try{
 const download=category=>({category,imported_category:null,recent_priority:0,older_priority:0});
 const clientSettings={implementation:'qbittorrent',endpoint:providerOrigin,tv:download('tv'),movies:download('movies')};
 const client=await api('post','/api/v1/providers',{name:'Binding browser client',enabled:true,priority:1,settings:clientSettings,credentials:{kind:'username_password',username:'fixture-user',password:'fixture-good'}},201);
 const alternative=await api('post','/api/v1/providers',{name:'Binding browser alternative',enabled:true,priority:2,settings:clientSettings,credentials:{kind:'username_password',username:'fixture-user',password:'fixture-good'}},201);
 client.alternativeId=alternative.id;
 const settings={implementation:'torznab',endpoint:`${providerOrigin}/torznab`,tv:{categories:[5030],anime_categories:[5070]},movies:{categories:[2000]}};
 const bound=await api('post','/api/v1/providers',{name:'Binding browser indexer',enabled:true,priority:1,settings,credentials:{kind:'api_key',api_key:'fixture-good'}},201);
 const unbound=await api('post','/api/v1/providers',{name:'Binding browser unbound',enabled:true,priority:2,settings,credentials:{kind:'api_key',api_key:'fixture-good'}},201);
 await page.goto(origin);await add('tv');await add('movies');await edit(bound);
 // All six policy flags and both bindings persist through a real editor write/readback.
 for(const prefix of ['TV','Movie']){
  await editor.getByLabel(new RegExp(`^${prefix} bound download client`)).selectOption(client.id);
  for(const flag of ['RSS','automatic search','interactive search'])await editor.getByLabel(`${prefix} ${flag}`,{exact:true}).uncheck();
 }
 await save();await editor.getByRole('button',{name:'Reload saved provider',exact:true}).click();await ready(editor.getByLabel('Name',{exact:true}));
 let stored=await readProvider(bound.id);
 for(const [domain,prefix] of [['tv','TV'],['movies','Movie']]){
  assert.equal(stored.settings[domain].download_client_id,client.id);
  assert.equal(await editor.getByLabel(new RegExp(`^${prefix} bound download client`)).inputValue(),client.id);
  for(const [flag,key] of [['RSS','enable_rss'],['automatic search','enable_automatic_search'],['interactive search','enable_interactive_search']]){
   assert.equal(stored.settings[domain][key],false);assert.equal(await editor.getByLabel(`${prefix} ${flag}`,{exact:true}).isChecked(),false);
   await editor.getByLabel(`${prefix} ${flag}`,{exact:true}).check();
  }
 }
 await save();stored=await readProvider(bound.id);
 for(const domain of ['tv','movies'])for(const key of ['enable_rss','enable_automatic_search','enable_interactive_search'])assert.equal(stored.settings[domain][key],true);
 for(const domain of ['tv','movies']){
  await openRss(domain);await checkSelection(rss,'RSS indexer','RSS download client',bound,unbound,client,true);
  const saved=page.waitForResponse(r=>r.request().method()==='POST'&&new URL(r.url()).pathname==='/api/v1/rss/schedules');
  await rss.getByRole('button',{name:'Save RSS schedule',exact:true}).click();const response=await saved;assert.equal(response.status(),200);
  const schedule=await response.json();assert.equal(schedule.target.client_id,client.id);assert.equal(schedule.target.media_type,domain);assert.equal(schedule.enabled,false);
  await openSearch(domain);await checkSelection(search,'Download search indexer','Search download client',bound,unbound,client,true);
  const accepted=page.waitForResponse(r=>r.request().method()==='POST'&&new URL(r.url()).pathname==='/api/v1/search/commands');
  await search.getByRole('button',{name:'Search to choose a download',exact:true}).click();const responseSearch=await accepted;assert.equal(responseSearch.status(),202);
  const command=await responseSearch.json();assert.equal(command.source.client_id,client.id);assert.equal(command.source.media_type,domain);
 }
 await updateProvider(client.id,{enabled:false});await edit(bound);
 for(const prefix of ['TV','Movie'])assert.equal(await editor.getByLabel(new RegExp(`^${prefix} bound download client`)).inputValue(),client.id);
 await editor.getByLabel('Priority',{exact:true}).fill('3');await save();
 stored=await readProvider(bound.id);for(const domain of ['tv','movies'])assert.equal(stored.settings[domain].download_client_id,client.id);
 for(const state of ['disabled','removed']){
  if(state==='removed'){
   const current=await readProvider(client.id);await api('delete',`/api/v1/providers/${client.id}?revision=${current.revision}`,undefined,204);
   await edit(bound);for(const prefix of ['TV','Movie'])assert.equal(await editor.getByLabel(new RegExp(`^${prefix} bound download client`)).inputValue(),client.id);
  }
  for(const domain of ['tv','movies']){
   await openRss(domain);await checkSelection(rss,'RSS indexer','RSS download client',bound,unbound,client,false);
   await openSearch(domain);await checkSelection(search,'Download search indexer','Search download client',bound,unbound,client,false);
  }
 }
 // A removed client remains visible until the user explicitly clears each binding.
 await edit(bound);
 for(const prefix of ['TV','Movie'])await editor.getByLabel(new RegExp(`^${prefix} bound download client`)).selectOption({label:`Any enabled ${prefix} client`});
 await save();stored=await readProvider(bound.id);
 for(const [domain,prefix] of [['tv','TV'],['movies','Movie']]){
  assert.equal(stored.settings[domain].download_client_id,null);
  assert.equal(await editor.getByLabel(new RegExp(`^${prefix} bound download client`)).inputValue(),'');
 }
 assert.deepEqual(errors,[]);
 console.log('PASS real TV/movie indexer binding and policy editor roundtrip; RSS schedule/search admission use bound client; disabled/removed client blocks actions and retains saved binding; unbound indexer restores explicit choice; user can explicitly clear both removed-client bindings.');
}finally{await browser.close();}
