// Owned library_ui_fixture only: real HTTP, no installed services. Backups are written into the
// fixture's scratch directory (default folder beside its database, then a scratch folder chosen in
// settings). Restores are only staged and then cancelled: the fixture never restarts, so live
// state is never replaced; restart application is covered by tests/backup_restore.rs.
import assert from 'node:assert/strict';
import {mkdir,readFile,stat} from 'node:fs/promises';
import {isAbsolute,join} from 'node:path';
import {pathToFileURL} from 'node:url';
const {chromium}=await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE||'/usr/lib/node_modules/playwright/index.mjs').href);
const origin=process.env.UI_URL,fixture=JSON.parse(process.env.UI_FIXTURE_JSON||'{}'),scratch=process.env.UI_SCRATCH;
assert.equal(new URL(origin).hostname,'127.0.0.1');
assert.equal(new URL(fixture.api).hostname,'127.0.0.1');
assert.ok(scratch&&isAbsolute(scratch),'UI_SCRATCH must be the owned fixture scratch directory');
const artifacts=process.env.UI_SCREENSHOT_DIR;
assert.ok(artifacts&&isAbsolute(artifacts)&&/\.do-not-commit\/loop\/iteration120(?:[-/]|$)/.test(artifacts),'UI_SCREENSHOT_DIR must be an owned iteration120 artifact directory');
await mkdir(artifacts,{recursive:true});
const browser=await chromium.launch({headless:true,...(process.env.PLAYWRIGHT_EXECUTABLE?{executablePath:process.env.PLAYWRIGHT_EXECUTABLE}:{})});
const page=await browser.newPage({acceptDownloads:true});page.setDefaultTimeout(20000);
const errors=[];page.on('pageerror',error=>errors.push(error.message));
page.on('dialog',dialog=>{errors.push(`Unexpected dialog: ${dialog.message()}`);void dialog.dismiss();});
async function api(method,path,data,status=200){
 const response=await page.request[method](origin+path,{...(data===undefined?{}:{data}),timeout:20000});
 assert.equal(response.status(),status,`${method} ${path}: ${await response.text()}`);return status===204?null:response.json();
}
async function until(read,accept,label,timeoutMs=20000){
 const deadline=Date.now()+timeoutMs;let last;
 while(Date.now()<deadline){last=await read();if(accept(last))return last;await page.waitForTimeout(100);}
 assert.fail(`${label}: ${JSON.stringify(last)}`);
}
const panel=page.getByRole('region',{name:'Backup',exact:true});
const table=panel.getByRole('table',{name:'Backups',exact:true});
const pendingRegion=panel.getByRole('region',{name:'Pending restore',exact:true});
async function rows(){await until(()=>table.getAttribute('aria-busy'),v=>v==='false','Backups table stayed busy');return table.getByRole('row').allInnerTexts();}
async function backupNow(){
 const response=page.waitForResponse(r=>r.request().method()==='POST'&&new URL(r.url()).pathname==='/api/v1/system/backup-commands');
 await panel.getByRole('button',{name:'Back up now',exact:true}).click();
 assert.equal((await response).status(),202);
 const status=panel.getByRole('status',{name:'Latest backup',exact:true});
 await until(()=>status.innerText(),t=>/^Manual backup (completed|failed)/.test(t),'Backup did not finish',30000);
 const text=await status.innerText();assert.match(text,/^Manual backup completed: hrrdarr_backup_\d{8}T\d{6}Z_[0-9a-f]{8}\.zip \(/,text);
 return text.match(/hrrdarr_backup_\S+\.zip/)[0];
}
async function pending(){return (await api('get','/api/v1/system/restore')).pending;}
try{
 // Make sure the shared command worker runs; the backup command is claimed by it.
 const started=await page.request.post(`${origin}/api/fixture/start-command-worker`);
 assert.ok([204,409].includes(started.status()),`worker start: ${started.status()}`);

 await page.goto(`${origin}/#/system/backup`);
 await panel.getByRole('heading',{name:'Backup',exact:true,level:2}).waitFor();
 assert.equal(await page.getByRole('navigation',{name:'Workspace'}).getByRole('button',{name:'Backup',exact:true}).getAttribute('aria-pressed'),'true');
 const settings=await api('get','/api/v1/system/backup-settings');
 assert.equal(settings.folder,null);assert.ok(settings.effective_folder.startsWith(scratch),settings.effective_folder);
 await panel.getByText(`Leave empty for the default (${settings.effective_folder})`,{exact:false}).waitFor();
 await panel.getByText('No backups yet.',{exact:true}).waitFor();
 assert.equal((await rows()).length,1,'header only');

 // Create two manual backups through the durable command.
 const first=await backupNow();
 await until(async()=>(await rows()).length,n=>n===2,'First backup listed');
 const second=await backupNow();
 await until(async()=>(await rows()).length,n=>n===3,'Second backup listed');
 let listed=(await api('get','/api/v1/system/backups')).items;
 assert.deepEqual(listed.map(b=>b.name).sort(),[first,second].sort());
 assert.ok(listed.every(b=>b.backup_type==='manual'&&/^[0-9a-f]{32}$/.test(b.id)));
 const byName=name=>listed.find(b=>b.name===name);
 await panel.getByText('Showing 1–2 of 2 backups.',{exact:true}).waitFor();
 await page.screenshot({path:join(artifacts,'backup-list-desktop.png'),fullPage:true});

 // Download streams the archive by id.
 const [download]=await Promise.all([page.waitForEvent('download'),table.getByRole('link',{name:`Download ${first}`,exact:true}).click()]);
 assert.equal(download.suggestedFilename(),first);
 const archive=await download.path();
 assert.equal((await stat(archive)).size,byName(first).size_bytes);
 const archiveBytes=await readFile(archive);
 assert.equal(archiveBytes.subarray(0,4).toString('latin1'),'PK\u0003\u0004','ZIP local header');

 // Delete needs explicit confirmation; cancel (and Escape) change nothing.
 await table.getByRole('button',{name:`Delete ${second}`,exact:true}).click();
 let dialog=panel.getByRole('alertdialog',{name:`Delete backup ${second}?`});
 await dialog.getByText('permanently removes this archive',{exact:false}).waitFor();
 assert.equal(await page.evaluate(()=>document.activeElement?.textContent),'Confirm delete','focus moves to the confirmation');
 await page.keyboard.press('Escape');
 assert.equal(await dialog.count(),0);
 await table.getByRole('button',{name:`Delete ${second}`,exact:true}).click();
 await dialog.getByRole('button',{name:'Cancel',exact:true}).click();
 assert.equal((await api('get','/api/v1/system/backups')).total,2,'cancelled delete keeps the backup');
 await table.getByRole('button',{name:`Delete ${second}`,exact:true}).click();
 await dialog.getByRole('button',{name:'Confirm delete',exact:true}).click();
 await panel.getByText(`Deleted ${second}.`,{exact:true}).waitFor();
 await until(async()=>(await rows()).length,n=>n===2,'Deleted backup leaves the list');
 assert.deepEqual((await api('get','/api/v1/system/backups')).items.map(b=>b.name),[first]);

 // Restore by id: explicit confirmation, validated and staged, applies only on restart.
 await table.getByRole('button',{name:`Restore ${first}`,exact:true}).click();
 dialog=panel.getByRole('alertdialog',{name:`Restore backup ${first}?`});
 await dialog.getByText('replaces the current database the next time hrrdarr restarts',{exact:false}).waitFor();
 await dialog.getByRole('button',{name:'Cancel',exact:true}).click();
 assert.equal(await pending(),null,'cancelled confirmation stages nothing');
 await table.getByRole('button',{name:`Restore ${first}`,exact:true}).click();
 await dialog.getByRole('button',{name:'Confirm restore',exact:true}).click();
 await panel.getByText('Restore validated and staged. Restart hrrdarr to apply it.',{exact:true}).waitFor();
 await pendingRegion.getByRole('heading',{name:'Restore pending — applies on restart'}).waitFor();
 let staged=await pending();
 assert.deepEqual([staged.source,staged.backup_id,staged.restart_required],['backup',byName(first).id,true]);
 assert.equal(await table.getByRole('button',{name:`Restore ${first}`,exact:true}).isDisabled(),true,'one pending restore at a time');
 await page.screenshot({path:join(artifacts,'backup-restore-pending-desktop.png'),fullPage:true});
 await pendingRegion.getByRole('button',{name:'Cancel pending restore',exact:true}).click();
 await panel.getByText('Pending restore cancelled. The current database stays in use.',{exact:true}).waitFor();
 assert.equal(await pending(),null);

 // Restore from an uploaded file: an invalid archive is refused with nothing staged.
 const fileInput=panel.getByLabel('Backup archive',{exact:true});
 await fileInput.setInputFiles({name:'bad.zip',mimeType:'application/zip',buffer:Buffer.from('definitely not an archive')});
 await panel.getByRole('button',{name:'Restore from file',exact:true}).click();
 dialog=panel.getByRole('alertdialog',{name:'Restore from bad.zip?'});
 await dialog.getByRole('button',{name:'Confirm restore',exact:true}).click();
 await panel.getByRole('alert').filter({hasText:'The file is not a valid hrrdarr backup archive. Nothing was changed.'}).waitFor();
 assert.equal(await pending(),null);
 await dialog.getByRole('button',{name:'Cancel',exact:true}).click();
 await fileInput.setInputFiles({name:'downloaded-backup.zip',mimeType:'application/zip',buffer:archiveBytes});
 await panel.getByRole('button',{name:'Restore from file',exact:true}).click();
 await panel.getByRole('alertdialog',{name:'Restore from downloaded-backup.zip?'}).getByRole('button',{name:'Confirm restore',exact:true}).click();
 await pendingRegion.getByRole('heading',{name:'Restore pending — applies on restart'}).waitFor();
 await pendingRegion.getByText('Restore of an uploaded archive',{exact:false}).waitFor();
 staged=await pending();assert.deepEqual([staged.source,staged.backup_id],['upload',null]);
 await pendingRegion.getByRole('button',{name:'Cancel pending restore',exact:true}).click();
 await until(pending,v=>v===null,'Upload restore cancelled');

 // Settings: client and server validation, then a new folder receives the next backup.
 const folder=panel.getByLabel('Backup folder',{exact:true});
 await folder.fill('relative/backups');
 await panel.getByRole('button',{name:'Save backup settings',exact:true}).click();
 await panel.getByRole('alert').filter({hasText:'Backup folder must be an absolute folder path'}).waitFor();
 // An existing scratch directory (root creation checks it is writable); only this script uses it.
 const root=`${scratch}/incoming`;
 await api('post','/api/v1/tv/root-folders',{path:root},201);
 await folder.fill(`${root}/archives`);
 await panel.getByRole('button',{name:'Save backup settings',exact:true}).click();
 await panel.getByRole('alert').filter({hasText:'cannot be, contain, or be inside a root folder'}).waitFor();
 assert.equal((await api('get','/api/v1/system/backup-settings')).revision,settings.revision,'refused writes change nothing');
 const chosen=`${scratch}/backup-browser-elsewhere`;
 await folder.fill(chosen);
 await panel.getByLabel('Scheduled backup interval (days)',{exact:true}).fill('3');
 await panel.getByLabel('Keep scheduled and update backups for (days)',{exact:true}).fill('10');
 await panel.getByRole('button',{name:'Save backup settings',exact:true}).click();
 await panel.getByText(`Backup settings saved. Backups are written to ${chosen}.`,{exact:true}).waitFor();
 const saved=await api('get','/api/v1/system/backup-settings');
 assert.deepEqual([saved.folder,saved.effective_folder,saved.interval_days,saved.retention_days],[chosen,chosen,3,10]);
 await panel.getByRole('button',{name:'Refresh backups',exact:true}).click();
 await panel.getByText('No backups yet.',{exact:true}).waitFor();
 const third=await backupNow();
 await until(async()=>(await rows()).length,n=>n===2,'Backup in the chosen folder listed');
 assert.equal((await stat(join(chosen,'manual',third))).isFile(),true);

 await page.setViewportSize({width:390,height:844});
 await rows();
 await page.screenshot({path:join(artifacts,'backup-mobile.png'),fullPage:true});
 assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1),'no horizontal overflow on mobile');
 assert.deepEqual(errors,[]);
 console.log(JSON.stringify({backup_browser:'passed',backups:[first,third],deleted:second}));
}finally{await browser.close();}
