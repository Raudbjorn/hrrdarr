// Real rename execution through the owned library_ui_fixture: preview list with checkboxes,
// Rename selected/all, series and movie folder rename, collision display, sidecars and the
// durable rename events. Files are real; nothing is mocked except the fixture's catalogue.
import assert from 'node:assert/strict';
import {mkdir,readdir,readFile,writeFile,stat} from 'node:fs/promises';
import {pathToFileURL} from 'node:url';
const origin=process.env.UI_URL,scratch=process.env.UI_SCRATCH;
assert.ok(origin&&scratch,'Set UI_URL and UI_SCRATCH');assert.equal(new URL(origin).hostname,'127.0.0.1');assert.ok(scratch.startsWith('/'));
const {chromium}=await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE||'/usr/lib/node_modules/playwright/index.mjs').href);
const browser=await chromium.launch({headless:true,...(process.env.PLAYWRIGHT_EXECUTABLE?{executablePath:process.env.PLAYWRIGHT_EXECUTABLE}:{})});
const page=await browser.newPage({viewport:{width:1280,height:900}});page.setDefaultTimeout(15000);
const errors=[],writes=[];page.on('pageerror',e=>errors.push(e.message));
page.on('request',r=>{const u=new URL(r.url());if(u.pathname.startsWith('/api/v1/')&&!['GET','HEAD'].includes(r.method()))writes.push(`${r.method()} ${u.pathname}`);});
const collection=page.getByRole('complementary',{name:'Library collection',exact:true});
const detail=page.getByRole('article',{name:'Library detail',exact:true});
const domain=page.getByRole('navigation',{name:'Library type',exact:true});
const tvPanel=page.getByRole('region',{name:'Series rename',exact:true});
const moviePanel=page.getByRole('region',{name:'Movie filename preview',exact:true});
async function request(method,path,body,status=200){const r=await page.request.fetch(`${origin}${path}`,{method,...(body===undefined?{}:{data:body})});assert.equal(r.status(),status,`${method} ${path}: ${await r.text()}`);return r.json();}
async function exists(p){try{await stat(p);return true;}catch{return false;}}
async function choose(title){await collection.getByRole('button',{name:new RegExp(`^${title}`)}).click();await detail.getByRole('heading',{name:title,exact:true}).waitFor();}
async function importFile(target,source,destination){const op=await request('POST','/api/v1/imports',{target,source,destination,mode:'copy'},202);assert.equal((await request('POST',`/api/v1/imports/${op.id}/execute`)).status,'complete');}
try{
 // ---- TV ----
 const tvRoot=`${scratch}/ren-tv`;await mkdir(tvRoot);
 const naming=await request('GET','/api/v1/tv/config/naming');
 await request('PUT','/api/v1/tv/config/naming',{...naming,rename_enabled:true,standard_episode_format:'{Series Title} - S{season:00}E{episode:00} - {Episode Title}',season_folder_format:'Season {season:00}',series_folder_format:'{Series Title} Renamed',multi_episode_style:4});
 const series=await request('POST','/api/v1/tv/series',{title:'Fixture series',tvdb_id:101,path:tvRoot},201);
 const eps=(await request('GET',`/api/v1/episodes?series_id=${series.id}&limit=25`)).items;
 assert.equal(eps.length,2,'fixture catalogue supplies two episodes');
 const source=`${scratch}/incoming/episode.mkv`;
 await importFile({media_type:'episode',id:eps[0].id},source,`${tvRoot}/pilot raw.mkv`);
 await importFile({media_type:'episode',id:eps[1].id},source,`${tvRoot}/second raw.mkv`);
 await writeFile(`${tvRoot}/pilot raw.en.srt`,'subtitle');
 await mkdir(`${tvRoot}/Season 01`);await writeFile(`${tvRoot}/Season 01/Fixture series - S01E02 - Second episode.mkv`,'squatter');
 const before=await readFile(`${tvRoot}/pilot raw.mkv`);
 await page.goto(origin);await choose('Fixture series');
 assert.equal(await tvPanel.getByText('pilot raw.mkv').count(),0,'Preview requires explicit intent');
 const preview=page.waitForResponse(r=>new URL(r.url()).pathname==='/api/v1/tv/rename-preview');
 await tvPanel.getByRole('button',{name:'Preview renames',exact:true}).click();assert.equal((await preview).status(),200);
 await tvPanel.getByText('Season 01/Fixture series - S01E01 - Pilot.mkv',{exact:true}).waitFor();
 await tvPanel.getByText('A file or folder already exists at the new path.').waitFor();
 assert.equal(await tvPanel.getByRole('checkbox').count(),1,'only renamable files can be selected');
 await tvPanel.getByRole('button',{name:'Rename selected (1)',exact:true}).waitFor();
 assert.equal(await exists(`${tvRoot}/pilot raw.mkv`),true,'preview did not touch disk');
 // Deselect then reselect: the selected-count button follows the checkbox.
 const box=tvPanel.getByRole('checkbox');await box.uncheck();await tvPanel.getByRole('button',{name:'Rename selected (0)',exact:true}).waitFor();
 assert.equal(await tvPanel.getByRole('button',{name:'Rename selected (0)',exact:true}).isDisabled(),true);
 await box.check();
 await tvPanel.getByRole('button',{name:'Rename selected (1)',exact:true}).click();
 await tvPanel.getByText(/Rename finished\. 1 renamed, 1 unchanged|Rename finished\. 1 renamed/).waitFor();
 assert.equal(await exists(`${tvRoot}/Season 01/Fixture series - S01E01 - Pilot.mkv`),true);
 assert.equal(await exists(`${tvRoot}/Season 01/Fixture series - S01E01 - Pilot.en.srt`),true,'sidecar moved with the media file');
 assert.equal(await exists(`${tvRoot}/pilot raw.mkv`),false);
 assert.deepEqual(await readFile(`${tvRoot}/Season 01/Fixture series - S01E01 - Pilot.mkv`),before,'bytes preserved');
 // The blocked file stays blocked and unmoved; removing the squatter makes it renamable via Rename all.
 await tvPanel.getByText('Blocked').waitFor();assert.equal(await exists(`${tvRoot}/second raw.mkv`),true);
 const { rm } = await import('node:fs/promises');await rm(`${tvRoot}/Season 01/Fixture series - S01E02 - Second episode.mkv`);
 await tvPanel.getByRole('button',{name:'Reload rename preview',exact:true}).click();
 await tvPanel.getByRole('button',{name:'Rename all',exact:true}).click();
 await tvPanel.getByText(/Rename finished\. 1 renamed/).waitFor();
 assert.equal(await exists(`${tvRoot}/Season 01/Fixture series - S01E02 - Second episode.mkv`),true);
 await tvPanel.getByText('No filename changes.').waitFor();
 // Folder rename.
 await tvPanel.getByText(`${scratch}/Fixture series Renamed`).waitFor();
 await tvPanel.getByRole('button',{name:'Rename series folder',exact:true}).click();
 await tvPanel.getByText(/Rename finished\. 1 renamed/).waitFor();
 assert.equal(await exists(`${scratch}/Fixture series Renamed/Season 01/Fixture series - S01E01 - Pilot.mkv`),true);
 assert.equal(await exists(tvRoot),false);
 assert.equal((await request('GET',`/api/v1/tv/series/${series.id}`)).path,`${scratch}/Fixture series Renamed`);
 await detail.getByText(`${scratch}/Fixture series Renamed`,{exact:true}).waitFor();
 // ---- Movies ----
 const movieRoot=`${scratch}/ren-movie`;await mkdir(movieRoot);
 const mnaming=await request('GET','/api/v1/movies/config/naming');
 await request('PUT','/api/v1/movies/config/naming',{...mnaming,rename_enabled:true,standard_movie_format:'{Movie Title}',movie_folder_format:'{Movie Title} Folder'});
 const movie=await request('POST','/api/v1/movies',{title:'Rename Film',tmdb_id:821,path:movieRoot},201);
 await importFile({media_type:'movie',id:movie.id},`${scratch}/incoming/movie.mkv`,`${movieRoot}/raw film.mkv`);
 await writeFile(`${movieRoot}/raw film.nfo`,'nfo');
 await domain.getByRole('button',{name:'Movies',exact:true}).click();await choose('Rename Film');
 await moviePanel.getByRole('button',{name:'Preview filenames',exact:true}).click();
 await moviePanel.getByText('Rename Film.mkv',{exact:true}).waitFor();
 await moviePanel.getByRole('button',{name:'Rename all',exact:true}).click();
 await moviePanel.getByText(/Rename finished\. 1 renamed/).waitFor();
 assert.equal(await exists(`${movieRoot}/Rename Film.mkv`),true);assert.equal(await exists(`${movieRoot}/Rename Film.nfo`),true);assert.equal(await exists(`${movieRoot}/raw film.mkv`),false);
 await moviePanel.getByText('No filename changes.').waitFor();
 await moviePanel.getByRole('button',{name:'Rename movie folder',exact:true}).click();
 await moviePanel.getByText(/Rename finished\. 1 renamed/).waitFor();
 assert.equal(await exists(`${scratch}/Rename Film Folder/Rename Film.mkv`),true);assert.equal(await exists(movieRoot),false);
 assert.equal((await request('GET',`/api/v1/movies/${movie.id}`)).path,`${scratch}/Rename Film Folder`);
 // Durable, domain-typed events: 2 TV files + series folder, 1 movie file + movie folder.
 const events=await request('GET','/api/v1/rename/events?limit=100');
 assert.equal(events.total,5);assert.deepEqual(new Set(events.items.map(e=>e.owner.media_type)),new Set(['tv','movies']));
 // Equal numeric IDs in both domains stayed independent: the TV series was not touched by the movie commands.
 assert.equal(events.items.filter(e=>e.owner.media_type==='tv'&&e.owner.id===series.id).length,3);
 // Narrow layout.
 await page.setViewportSize({width:390,height:844});assert.equal(await moviePanel.evaluate(n=>n.scrollWidth<=n.clientWidth+1),true,'rename panel fits narrow screens');
 assert.ok(writes.filter(w=>w==='POST /api/v1/rename/commands').length===5,`five explicit command submissions, saw ${JSON.stringify(writes)}`);
 assert.deepEqual(errors,[]);
 console.log('PASS rename: TV preview/select/blocked/all/folder, movie all/folder, sidecars, events, narrow layout');
}finally{await browser.close();}
