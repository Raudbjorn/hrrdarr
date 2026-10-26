import assert from 'node:assert/strict';
import {pathToFileURL} from 'node:url';
const {chromium}=await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE||'/usr/lib/node_modules/playwright/index.mjs').href);
const origin=process.env.UI_URL;assert.ok(origin);assert.equal(new URL(origin).hostname,'127.0.0.1');
const browser=await chromium.launch({headless:true,...(process.env.PLAYWRIGHT_EXECUTABLE?{executablePath:process.env.PLAYWRIGHT_EXECUTABLE}:{})}),page=await browser.newPage();page.setDefaultTimeout(15000);
let acceptDialogs=true;page.on('dialog',dialog=>acceptDialogs?dialog.accept():dialog.dismiss());
const errors=[];page.on('pageerror',error=>errors.push(error.message));
const nav=page.getByRole('navigation',{name:'Workspace'}),panel=page.getByRole('region',{name:'Import policy',exact:true});
const save=panel.getByRole('button',{name:'Save import policy',exact:true}),reload=panel.getByRole('button',{name:'Reload import policy',exact:true});
const minimum=panel.getByLabel('Minimum free space (MiB)',{exact:true}),folders=panel.getByLabel('Download-client working folder prefixes',{exact:true}),skip=panel.getByLabel('Skip free-space check when importing',{exact:true});
async function read(domain){const response=await page.request.get(`${origin}/api/v1/${domain}/import-policy`);assert.equal(response.status(),200);return response.json();}
async function ready(){await page.waitForFunction(()=>{const fieldset=document.querySelector('section[aria-label="Import policy"] fieldset');return fieldset&&!fieldset.disabled;});}
try {
  await page.goto(origin);await nav.getByRole('button',{name:'Media management',exact:true}).click();await ready();
  await panel.getByText('Import workers do not use these settings yet',{exact:false}).waitFor();
  for(const domain of ['tv','movies']) {
    if(domain==='movies'){await panel.getByRole('button',{name:'Movies',exact:true}).click();await ready();}
    const other=domain==='tv'?'movies':'tv',untouched=await read(other);
    await folders.fill('unsaved literal||prefix');
    acceptDialogs=false;
    await panel.getByRole('button',{name:domain==='tv'?'Movies':'TV',exact:true}).click();
    assert.equal(await folders.inputValue(),'unsaved literal||prefix');
    assert.equal(await panel.getByRole('button',{name:domain==='tv'?'TV':'Movies',exact:true}).getAttribute('aria-pressed'),'true');
    acceptDialogs=true;
    await nav.getByRole('button',{name:'Library',exact:true}).click();await nav.getByRole('button',{name:'Media management',exact:true}).click();assert.equal(await folders.inputValue(),'unsaved literal||prefix');
    await minimum.fill('99');assert.equal(await minimum.evaluate(input=>input.checkValidity()),false);
    await minimum.fill('2147483648');assert.equal(await minimum.evaluate(input=>input.checkValidity()),false);
    const freeSpace=domain==='tv'?100:2147483647;
    await minimum.fill(String(freeSpace));await skip.setChecked(domain==='tv');
    await folders.fill('é'.repeat(2048)+'x');await save.click();await panel.getByText('Download-client working folder prefixes must fit within 4096 UTF-8 bytes.',{exact:true}).waitFor();
    for(const value of [' _UNPACK_||_failed_| ', 'tab\t|line\n||end ', '', ' ']) {
      const before=await read(domain);await folders.fill(value);await save.click();await panel.getByText('Import policy saved.',{exact:true}).waitFor();
      const after=await read(domain);assert.equal(after.revision,before.revision+1);assert.equal(after.minimum_free_space_mib,freeSpace);assert.equal(after.skip_free_space_check_when_importing,domain==='tv');assert.equal(after.download_client_working_folders,value===''?'_UNPACK_|_FAILED_':value);assert.equal(await folders.inputValue(),after.download_client_working_folders);assert.deepEqual(await read(other),untouched);
    }
    const before=await read(domain);await folders.fill('retained draft||');
    const response=await page.request.put(`${origin}/api/v1/${domain}/import-policy`,{data:{revision:before.revision,minimum_free_space_mib:123,skip_free_space_check_when_importing:false,download_client_working_folders:'concurrent'}});assert.equal(response.status(),200);
    await save.click();await panel.getByText('Import policy changed in another request.',{exact:false}).waitFor();assert.equal(await folders.inputValue(),'retained draft||');assert.equal(await save.isDisabled(),true);
    await reload.click();await panel.getByText('Saved import policy loaded.',{exact:true}).waitFor();assert.equal(await folders.inputValue(),'concurrent');
    const pattern=`**/api/v1/${domain}/import-policy`;
    for(const invalid of [{...await read(other),media_type:other},null]) {
      await folders.fill('retained after mismatched read||');
      await page.route(pattern,route=>route.request().method()==='GET'?route.fulfill({status:200,contentType:'application/json',body:JSON.stringify(invalid)}):route.continue());
      await reload.click();await panel.getByText('The saved policy response does not match this media type. Reload before editing.',{exact:true}).waitFor();
      assert.equal(await folders.inputValue(),'retained after mismatched read||');assert.equal(await save.isDisabled(),true);
      await page.unroute(pattern);await reload.click();await panel.getByText('Saved import policy loaded.',{exact:true}).waitFor();await ready();assert.equal(await folders.inputValue(),'concurrent');
    }
    let captured;
    const held=new Promise(resolve=>captured=resolve);
    await page.route(pattern,route=>route.request().method()==='PUT'?captured(route):route.continue());
    await folders.fill('committed||literal ');await save.click();const route=await held;
    await nav.getByRole('button',{name:'Library',exact:true}).click();await nav.getByRole('button',{name:'Media management',exact:true}).click();assert.equal(await save.isDisabled(),true);
    const committed=await route.fetch();assert.equal(committed.status(),200);await route.abort('failed');await panel.getByText('Write outcome unknown.',{exact:false}).waitFor();assert.equal(await folders.inputValue(),'committed||literal ');assert.equal(await save.isDisabled(),true);
    assert.equal(await panel.getByRole('button',{name:domain==='tv'?'Movies':'TV',exact:true}).isDisabled(),true);
    await page.unroute(pattern);
    await page.route(pattern,route=>route.fulfill({status:503,contentType:'application/json',body:JSON.stringify({error:{code:'fixture_read_failure',message:'Owned readback failure'}})}));
    await reload.click();await panel.getByText('Owned readback failure',{exact:true}).waitFor();assert.equal(await save.isDisabled(),true);assert.equal(await folders.inputValue(),'committed||literal ');
    await page.unroute(pattern);await reload.click();await panel.getByText('Saved import policy loaded.',{exact:true}).waitFor();await ready();assert.equal((await read(domain)).download_client_working_folders,'committed||literal ');assert.deepEqual(await read(other),untouched);
  }
  assert.deepEqual(errors,[]);console.log('Import policy browser passed TV/movie isolation, MiB/UTF-8 validation, literal/default roundtrip, draft navigation, CAS conflicts and committed lost-write readback.');
} finally {await browser.close();}
