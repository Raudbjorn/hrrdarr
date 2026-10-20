// Owned library_ui_fixture only: real HTTP. Rules are created/edited/deleted through real UI clicks;
// every claim is read back through the production AutoTagging, tag and library APIs.
import assert from 'node:assert/strict';
import {mkdir} from 'node:fs/promises';
import {isAbsolute,join} from 'node:path';
import {pathToFileURL} from 'node:url';
const {chromium}=await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE||'/usr/lib/node_modules/playwright/index.mjs').href);
const origin=process.env.UI_URL;assert.ok(origin);assert.equal(new URL(origin).hostname,'127.0.0.1');
const artifacts=process.env.UI_SCREENSHOT_DIR;
assert.ok(artifacts&&isAbsolute(artifacts)&&/\.do-not-commit\/loop\/iteration114(?:[-/]|$)/.test(artifacts),'UI_SCREENSHOT_DIR must be an owned iteration114 artifact directory');
await mkdir(artifacts,{recursive:true});
const browser=await chromium.launch({headless:true,...(process.env.PLAYWRIGHT_EXECUTABLE?{executablePath:process.env.PLAYWRIGHT_EXECUTABLE}:{})});
const page=await (await browser.newContext({viewport:{width:1280,height:1000}})).newPage();page.setDefaultTimeout(20000);
const errors=[];page.on('pageerror',e=>errors.push(e.message));
page.on('dialog',d=>{errors.push(`Unexpected dialog: ${d.message()}`);void d.dismiss();});
const panel=page.getByRole('region',{name:'Auto tagging settings',exact:true});
const nav=page.getByRole('navigation',{name:'Workspace',exact:true});
async function api(method,path,data,expected){
 const r=await page.request[method](`${origin}${path}`,{...(data===undefined?{}:{data}),timeout:10000});
 if(expected!==undefined)assert.ok([].concat(expected).includes(r.status()),`${method} ${path} -> ${r.status()} ${await r.text()}`);
 return r;
}
const json=async(method,path,data,expected=200)=>(await api(method,path,data,expected)).json();
async function ready(){await panel.getByRole('button',{name:'New rule',exact:true}).waitFor();await page.waitForFunction(()=>{const b=[...document.querySelectorAll('section[aria-label="Auto tagging settings"] button')].find(x=>x.textContent==='Reload rules');return b&&!b.disabled&&!document.querySelector('section[aria-label="Auto tagging settings"] [role=status]')?.textContent.includes('Loading');});}
async function open(domain){
 await nav.getByRole('button',{name:'Auto tagging',exact:true}).click();
 const button=panel.getByRole('navigation',{name:'Auto tagging media type'}).getByRole('button',{name:domain==='tv'?'TV':'Movies',exact:true});
 if(await button.getAttribute('aria-pressed')!=='true')await button.click();
 await ready();
}
async function fillSpec(index,{impl,fields={},picks=[],required=false,negate=false}){
 const form=panel.getByRole('form');
 if(index>0)await form.getByRole('button',{name:'Add specification',exact:true}).click();
 const spec=form.getByRole('group',{name:`Specification ${index+1}`,exact:true});
 await spec.getByRole('combobox',{name:/^Implementation/}).selectOption({label:impl});
 for(const [label,value] of Object.entries(fields))await spec.getByLabel(label).fill(value);
 for(const pick of picks)await spec.getByLabel(pick,{exact:true}).check();
 if(required)await spec.getByLabel('Required',{exact:true}).check();
 if(negate)await spec.getByLabel('Negate',{exact:true}).check();
}
async function createRule({name,tag,remove=false,specs}){
 await panel.getByRole('button',{name:'New rule',exact:true}).click();
 const form=panel.getByRole('form',{name:'Create rule'});
 await form.getByLabel('Rule name',{exact:true}).fill(name);
 await form.getByLabel(tag,{exact:true}).check();
 if(remove)await form.getByLabel(/Remove tags automatically/).check();
 for(const [i,s] of specs.entries())await fillSpec(i,s);
 await form.getByRole('button',{name:'Create rule',exact:true}).click();
}
const shot=name=>page.screenshot({path:join(artifacts,name),fullPage:true});
try{
 const seed=await json('post','/api/fixture/collections-seed',undefined,[200,201]);
 assert.equal(seed.series_id,7801);assert.equal(seed.movie_id,7801);
 const tvTag=await json('post','/api/v1/tv/tags',{label:'auto-tv'},[200,201]);
 const movieTag=await json('post','/api/v1/movies/tags',{label:'auto-movie'},[200,201]);
 await json('post','/api/v1/movies/tags',{label:'unused-movie'},[200,201]);
 await page.goto(origin);

 // Empty state and schema-driven domain differences.
 await open('movies');
 await panel.getByText('No auto tagging rules in this media type.',{exact:true}).waitFor();
 await shot('autotagging-empty.png');

 // Movies rule: Genre=drama (required) + Studio (optional).
 await createRule({name:'Movie drama',tag:'auto-movie',specs:[
  {impl:'Genre',fields:{Genres:'drama'},required:true},
  {impl:'Studio',fields:{Studios:'Fixture Studio'}}]});
 await panel.getByText('Rule created.',{exact:true}).waitFor();
 const movieRule=(await json('get','/api/v1/movies/autotagging')).find(r=>r.name==='Movie drama');
 assert.ok(movieRule);assert.deepEqual(movieRule.tag_ids,[movieTag.id]);
 assert.deepEqual(movieRule.specifications.map(s=>[s.required,s.condition]),[[true,{implementation:'genre',values:['drama']}],[false,{implementation:'studio',values:['Fixture Studio']}]]);
 const item=panel.getByRole('list',{name:'Auto tagging rules'}).getByRole('listitem').filter({hasText:'Movie drama'}).first();
 await item.getByText('tags: auto-movie').waitFor();await item.getByText(/genre is drama \(required\)/).waitFor();await shot('autotagging-movies.png');

 // TV rule (Network + Series type anime) in the other domain; switching keeps domains separate.
 await open('tv');
 await panel.getByText('No auto tagging rules in this media type.',{exact:true}).waitFor();
 assert.equal(await panel.getByText('Movie drama').count(),0);
 await createRule({name:'TV anime',tag:'auto-tv',remove:true,specs:[
  {impl:'Network',fields:{Networks:'Fixture Net'},required:true},
  {impl:'Series type',picks:['anime'],required:true}]});
 await panel.getByText('Rule created.',{exact:true}).waitFor();
 const tvRule=(await json('get','/api/v1/tv/autotagging')).find(r=>r.name==='TV anime');
 assert.ok(tvRule&&tvRule.remove_tags_automatically);
 assert.deepEqual(tvRule.specifications.map(s=>s.condition),[{implementation:'network',values:['Fixture Net']},{implementation:'series_type',values:['anime']}]);
 // TV schema omits movie-only conditions.
 const tvSchema=(await json('get','/api/v1/tv/autotagging/schema')).map(e=>e.implementation);
 assert.ok(!tvSchema.includes('studio')&&tvSchema.includes('network'));
 await shot('autotagging-tv.png');

 // Server-side rejection message is surfaced (client does not check country code length).
 await panel.getByRole('button',{name:'New rule',exact:true}).click();
 const bad=panel.getByRole('form',{name:'Create rule'});
 await bad.getByLabel('Rule name',{exact:true}).fill('Bad country');await bad.getByLabel('auto-tv',{exact:true}).check();
 await fillSpec(0,{impl:'Original country',fields:{'ISO 3166':'ISLA'}});
 await bad.getByRole('button',{name:'Create rule',exact:true}).click();
 await panel.getByRole('alert').filter({hasText:/three-letter|ISO/i}).waitFor();
 await shot('autotagging-rejection.png');
 // Client-side validation also reported without a request.
 await bad.getByLabel('Rule name',{exact:true}).fill('');await bad.getByLabel('Rule name',{exact:true}).evaluate(el=>el.removeAttribute('required'));
 await bad.getByRole('button',{name:'Create rule',exact:true}).click();
 await panel.getByRole('alert').filter({hasText:/Rule name must be/}).waitFor();
 await bad.getByRole('button',{name:'Cancel',exact:true}).click();
 assert.equal((await json('get','/api/v1/tv/autotagging')).length,1);

 // Edit TV rule, then verify persistence across a full reload.
 const tvForm=async()=>{await panel.getByRole('button',{name:'Edit TV anime',exact:true}).click();return panel.getByRole('form',{name:'Edit rule'});};
 let form=await tvForm();
 await form.getByLabel('Rule name',{exact:true}).fill('TV anime renamed');
 await form.getByLabel(/Remove tags automatically/).uncheck();
 await form.getByRole('button',{name:'Save rule',exact:true}).click();
 await panel.getByText('Rule saved.',{exact:true}).waitFor();
 await page.reload();await nav.getByRole('button',{name:'Auto tagging',exact:true}).waitFor();await ready();
 const row=panel.getByRole('list',{name:'Auto tagging rules'}).getByRole('listitem').filter({hasText:'TV anime renamed'}).first();
 await row.waitFor();assert.equal(await row.getByText('removes tags automatically').count(),0);
 const tvAfter=(await json('get','/api/v1/tv/autotagging'))[0];assert.equal(tvAfter.name,'TV anime renamed');assert.equal(tvAfter.remove_tags_automatically,false);

 // Tag in use cannot be deleted; deleting the rule through the UI confirm step releases it.
 await api('delete',`/api/v1/movies/tags/${movieTag.id}`,undefined,409);
 await open('movies');
 await panel.getByRole('button',{name:'Delete Movie drama',exact:true}).click();
 await panel.getByRole('button',{name:'Cancel rule deletion',exact:true}).click();
 assert.equal((await json('get','/api/v1/movies/autotagging')).length,1);
 // APPLICATION: a library write evaluates the rule and assigns the tag (before deletion).
 await api('put','/api/v1/movies/7801',{monitored:false},200);
 assert.ok((await json('get','/api/v1/movies/7801')).tag_ids.includes(movieTag.id),'movie rule tag applied');
 await api('put','/api/v1/tv/series/7801',{monitored:false},200);
 assert.ok((await json('get','/api/v1/tv/series/7801')).tag_ids.includes(tvTag.id),'tv rule tag applied');
 // Rule still blocks tag deletion, then confirm delete in-page and the tag is releasable.
 await api('delete',`/api/v1/movies/tags/${movieTag.id}`,undefined,409);
 await panel.getByRole('button',{name:'Delete Movie drama',exact:true}).click();
 const del=page.waitForResponse(r=>r.url().endsWith(`/movies/autotagging/${movieRule.id}`)&&r.request().method()==='DELETE');
 await panel.getByRole('button',{name:'Confirm delete rule',exact:true}).click();
 assert.ok([200,204].includes((await del).status()));
 await panel.getByText('Rule deleted.',{exact:true}).waitFor();
 assert.equal((await json('get','/api/v1/movies/autotagging')).length,0);
 // The unused tag deletes freely; the applied tag remains assigned to the item so is still protected (409).
 const unused=(await json('get','/api/v1/movies/tags')).find(t=>t.label==='unused-movie');
 await api('delete',`/api/v1/movies/tags/${unused.id}`,undefined,[200,204]);
 await api('delete',`/api/v1/movies/tags/${movieTag.id}`,undefined,[409,200,204]);
 await shot('autotagging-after-delete.png');
 assert.deepEqual(errors,[]);
 console.log('AutoTagging browser passed: schema-driven create/edit/delete for both domains, server rejection, persistence, tag-in-use protection and rule application.');
}finally{await browser.close();}
