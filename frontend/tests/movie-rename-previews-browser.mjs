// Parent-owned library_ui_fixture + Vite proxy. This script only reads its owned scratch files.
import assert from 'node:assert/strict';
import {readFile,stat} from 'node:fs/promises';
import {pathToFileURL} from 'node:url';
const origin=process.env.UI_URL,scratch=process.env.UI_SCRATCH;
assert.ok(origin&&scratch,'Set UI_URL and UI_SCRATCH=fixture.scratch');assert.equal(new URL(origin).hostname,'127.0.0.1');assert.ok(scratch.startsWith('/'));
const {chromium}=await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE||'/usr/lib/node_modules/playwright/index.mjs').href);
const browser=await chromium.launch({headless:true});const page=await browser.newPage();page.setDefaultTimeout(10000);
const errors=[],writes=[];page.on('pageerror',error=>errors.push(error.message));
const panel=page.getByRole('region',{name:'Movie filename preview',exact:true});
const collection=page.getByRole('complementary',{name:'Library collection',exact:true});
const detail=page.getByRole('article',{name:'Library detail',exact:true});
const domain=page.getByRole('navigation',{name:'Library type',exact:true});
const endpoint='/api/v1/movies/rename-preview',pattern='**/api/v1/movies/rename-preview?*';
async function request(method,path,body,status=200){const response=await page.request.fetch(`${origin}${path}`,{method,...(body===undefined?{}:{data:body})});assert.equal(response.status(),status,`${method} ${path}: ${await response.text()}`);return response.json();}
async function configure(enabled,format){const config=await request('GET','/api/v1/movies/config/naming');return request('PUT','/api/v1/movies/config/naming',{...config,rename_enabled:enabled,standard_movie_format:format});}
async function choose(title){await collection.getByRole('button',{name:new RegExp(`^${title}`)}).click();await detail.getByRole('heading',{name:title,exact:true}).waitFor();}
async function reload(){const response=page.waitForResponse(r=>new URL(r.url()).pathname===endpoint);await panel.getByRole('button',{name:'Reload filename preview',exact:true}).click();return response;}
async function bounded(promise,label){let timer;try{return await Promise.race([promise,new Promise((_,reject)=>{timer=setTimeout(()=>reject(new Error(label)),10000);})]);}finally{clearTimeout(timer);}}
let release;
try{
 const movie=await request('POST','/api/v1/movies',{title:'Preview Film',tmdb_id:811,path:`${scratch}/movies`},201);
 const other=await request('POST','/api/v1/movies',{title:'Empty Preview',tmdb_id:812,path:`${scratch}/search-movies-202`},201);
 const unchanged=await request('POST','/api/v1/movies',{title:'Unchanged Preview',tmdb_id:813,path:`${scratch}/search-movies-203`},201);
 const tv=await request('POST','/api/v1/tv/series',{title:'Preview TV',tvdb_id:811,path:`${scratch}/tv`},201);
 const source=`${scratch}/incoming/movie.mkv`,destination=`${scratch}/movies/<original>.mkv`;
 const operation=await request('POST','/api/v1/imports',{target:{media_type:'movie',id:movie.id},source,destination,mode:'copy'},202);
 assert.equal((await request('POST',`/api/v1/imports/${operation.id}/execute`)).status,'complete');
 const secondImport=await request('POST','/api/v1/imports',{target:{media_type:'movie',id:unchanged.id},source,destination:`${scratch}/search-movies-203/Exact.mkv`,mode:'copy'},202);
 assert.equal((await request('POST',`/api/v1/imports/${secondImport.id}/execute`)).status,'complete');
 const originalBytes=await readFile(source),destinationBytes=await readFile(destination),originalStat=await stat(destination);
 const files=await request('GET',`/api/v1/movies/files?movie_ids=${movie.id}`);assert.equal(files.items.length,1);
 const file=files.items[0];
 // No fixture mutation route: real manual import populated immutable original source facts.
 const disabled=await request('GET',`${endpoint}?movie_ids=${movie.id}`);assert.equal(disabled.rename_enabled,false);assert.equal(disabled.items[0].new_path,'movie.mkv');
 const saved=await request('GET',`/api/v1/movies/${movie.id}`);
 page.on('request',r=>{if(new URL(r.url()).pathname.startsWith('/api/v1/')&&!['GET','HEAD'].includes(r.method()))writes.push([r.method(),new URL(r.url()).pathname]);});
 await page.goto(origin);await domain.getByRole('button',{name:'Movies',exact:true}).click();await choose('Preview Film');
 assert.equal(await panel.getByText('movie.mkv',{exact:true}).count(),0,'Preview requires explicit intent');
 const initial=page.waitForResponse(r=>new URL(r.url()).pathname===endpoint);
 const previewButton=panel.getByRole('button',{name:'Preview filenames',exact:true});await previewButton.focus();await previewButton.press('Enter');assert.equal((await initial).status(),200);
 await panel.getByText('movie.mkv',{exact:true}).waitFor();await panel.getByText(/Renaming is disabled/).waitFor();
 await panel.getByText('<original>.mkv',{exact:true}).waitFor();assert.equal(await panel.locator('original,img,script').count(),0);
 await panel.getByText('Preview only. Destination occupancy, symlinks, and execution safety are not checked.',{exact:true}).waitFor();
 assert.equal(await panel.getByRole('button',{name:/^(Rename|Apply|Execute)/}).count(),0);
 await detail.getByText('Library settings',{exact:true}).click();await detail.getByLabel('Minimum availability',{exact:true}).selectOption('announced');
 const configured=await configure(true,'{Movie Title}');assert.equal((await reload()).status(),200);await panel.getByText('Preview Film.mkv',{exact:true}).waitFor();
 await panel.getByText(`Naming revision ${configured.revision}. Paths are relative to this movie’s folder.`,{exact:true}).waitFor();
 assert.equal(await detail.getByLabel('Minimum availability',{exact:true}).inputValue(),'announced');assert.deepEqual(await request('GET',`/api/v1/movies/${movie.id}`),saved);
 // A token with an absent optional fact gives an actual per-file unavailable result, not global empty success.
 await configure(true,'{Edition Tags}');assert.equal((await reload()).status(),200);
 await panel.getByText('This file cannot produce a safe name from the naming pattern.',{exact:true}).waitFor();await panel.getByText('1 files considered; 0 unchanged; 1 unavailable.',{exact:true}).waitFor();
 await configure(true,'{Movie Title}');await reload();await panel.getByText('Preview Film.mkv',{exact:true}).waitFor();
 // Natural unchanged results are separate from a movie with no file.
 await configure(true,'Exact');await choose('Unchanged Preview');await panel.getByRole('button',{name:'Preview filenames',exact:true}).click();await panel.getByText('No filename changes.',{exact:true}).waitFor();await panel.getByText('1 files considered; 1 unchanged; 0 unavailable.',{exact:true}).waitFor();
 await configure(true,'{Movie Title}');
 await choose('Empty Preview');await panel.getByRole('button',{name:'Preview filenames',exact:true}).click();await panel.getByText('No movie files to preview.',{exact:true}).waitFor();
 await choose('Preview Film');await panel.getByRole('button',{name:'Preview filenames',exact:true}).click();await panel.getByText('Preview Film.mkv',{exact:true}).waitFor();
 await page.route(pattern,r=>r.fulfill({status:503,contentType:'application/json',body:JSON.stringify({error:{code:'rename_preview_unavailable',message:'Owned preview unavailable'}})}));
 await reload();await panel.getByRole('alert').waitFor();assert.match(await panel.getByRole('alert').innerText(),/Owned preview unavailable/);assert.equal(await panel.getByText('Preview Film.mkv',{exact:true}).count(),0);
 await page.unroute(pattern);await reload();await panel.getByText('Preview Film.mkv',{exact:true}).waitFor();
 // Closing invalidates an in-flight real response without changing any naming facts.
 let closeEntered;const closeStarted=new Promise(resolve=>closeEntered=resolve),closeHeld=new Promise(resolve=>release=resolve);
 await page.route(pattern,async r=>{const response=await r.fetch();closeEntered();await closeHeld;await r.fulfill({response});});
 await panel.getByRole('button',{name:'Reload filename preview',exact:true}).click();await bounded(closeStarted,'Close request did not reach gate');
 await panel.getByRole('button',{name:'Close filename preview',exact:true}).click();await panel.getByRole('button',{name:'Preview filenames',exact:true}).waitFor();
 const closeResponse=page.waitForResponse(r=>new URL(r.url()).pathname===endpoint);release();await closeResponse;await page.unroute(pattern);
 await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve))));assert.equal(await panel.getByText('Preview Film.mkv',{exact:true}).count(),0);await panel.getByRole('button',{name:'Preview filenames',exact:true}).click();await panel.getByText('Preview Film.mkv',{exact:true}).waitFor();
 let entered;const enteredPromise=new Promise(resolve=>entered=resolve),held=new Promise(resolve=>release=resolve);
 await page.route(pattern,async r=>{if(new URL(r.request().url()).searchParams.get('movie_ids')!==String(movie.id))return r.continue();const response=await r.fetch();entered();await held;await r.fulfill({response});});
 await panel.getByRole('button',{name:'Reload filename preview',exact:true}).click();await bounded(enteredPromise,'Old movie request did not reach gate');await panel.getByText('Loading filename preview…',{exact:true}).waitFor();assert.equal(await panel.getByRole('button',{name:'Reload filename preview',exact:true}).isDisabled(),true);
 await choose('Empty Preview');await panel.getByRole('button',{name:'Preview filenames',exact:true}).click();await panel.getByText('No movie files to preview.',{exact:true}).waitFor();
 const stale=page.waitForResponse(r=>new URL(r.url()).pathname===endpoint&&new URL(r.url()).searchParams.get('movie_ids')===String(movie.id));release();await stale;await page.unroute(pattern);
 await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve))));assert.equal(await panel.getByText('Preview Film.mkv',{exact:true}).count(),0);await panel.getByText('No movie files to preview.',{exact:true}).waitFor();
 let enteredAgain;const started=new Promise(resolve=>enteredAgain=resolve),heldAgain=new Promise(resolve=>release=resolve);
 await page.route(pattern,async r=>{enteredAgain();await heldAgain;await r.fulfill({status:500,contentType:'application/json',body:JSON.stringify({error:{code:'database_error',message:'Stale movie failure'}})});});
 await panel.getByRole('button',{name:'Reload filename preview',exact:true}).click();await bounded(started,'Domain request did not reach gate');await domain.getByRole('button',{name:'TV',exact:true}).click();await choose('Preview TV');
 const oldFailure=page.waitForResponse(r=>new URL(r.url()).pathname===endpoint);release();await oldFailure;await page.unroute(pattern);await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve))));assert.equal(await panel.count(),0);assert.equal(await page.getByText('Stale movie failure',{exact:true}).count(),0);
 await page.reload();await domain.getByRole('button',{name:'Movies',exact:true}).click();await choose('Preview Film');await panel.getByRole('button',{name:'Preview filenames',exact:true}).click();await panel.getByText('Preview Film.mkv',{exact:true}).waitFor();
 await page.setViewportSize({width:390,height:844});assert.equal(await panel.evaluate(node=>node.scrollWidth<=node.clientWidth+1),true,'Preview paths fit the narrow panel');
 assert.deepEqual(await request('GET',`/api/v1/movies/files/${file.id}`),file);assert.deepEqual(await readFile(source),originalBytes);assert.deepEqual(await readFile(destination),destinationBytes);assert.equal((await stat(destination)).ino,originalStat.ino);
 assert.deepEqual(writes,[],'Only setup/configuration uses explicit API writes; preview controls never write');assert.deepEqual(errors,[]);
 console.log('PASS movie rename preview: actual imported file, disabled/enabled/unavailable/empty, keyboard, text escaping, dirty settings, error retry and stale movie/TV fences');
}finally{release?.();await browser.close();}
