import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {parseHash,hashFor,VIEW_IDS,VIEW_PATHS,SETTINGS_SECTIONS,UNAVAILABLE_SECTIONS,MAX_DISPLAY_LENGTH,MAX_HASH_LENGTH} from '../src/lib/routes.ts';

const EXPECTED_VIEWS=['library','health','providers','activity','blocklist','rss','qualities','profiles','naming','import-existing','custom-formats','tags','media-management','general','history'];

test('table covers exactly the existing views',()=>{
  assert.deepEqual([...VIEW_IDS].sort(),[...EXPECTED_VIEWS].sort());
});

test('every view round-trips and has a unique path',()=>{
  const paths=new Set();
  for(const view of VIEW_IDS){
    assert.deepEqual(parseHash(hashFor(view)),{kind:'view',view},view);
    paths.add(VIEW_PATHS[view]);
  }
  assert.equal(paths.size,VIEW_IDS.length);
  assert.equal(hashFor('library'),'#/');
});

test('empty hashes map to library',()=>{
  for(const h of ['','#','#/','/']) assert.deepEqual(parseHash(h),{kind:'view',view:'library'},h);
});

test('settings index with and without trailing slash',()=>{
  for(const h of ['#/settings','#/settings/','#/settings?x=1']) assert.deepEqual(parseHash(h),{kind:'settings-index'},h);
});

test('normalization: trailing slash, query, case sensitivity',()=>{
  assert.deepEqual(parseHash('#/settings/tags/'),{kind:'view',view:'tags'});
  assert.deepEqual(parseHash('#/activity/queue?page=2'),{kind:'view',view:'activity'});
  assert.equal(parseHash('#/Settings/Tags').kind,'not-found');
  assert.equal(parseHash('#/settings//tags').kind,'not-found');
  assert.equal(parseHash('#/settings%2Ftags').kind,'not-found');
  assert.deepEqual(parseHash('#/settings/%74ags'),{kind:'view',view:'tags'});
});

test('unknown paths are not-found with a display path',()=>{
  assert.deepEqual(parseHash('#/nope'),{kind:'not-found',path:'/nope'});
  assert.deepEqual(parseHash('#/settings/nope/deeper'),{kind:'not-found',path:'/settings/nope/deeper'});
});

test('overlong hashes are rejected and display is bounded',()=>{
  const r=parseHash('#/'+'a'.repeat(MAX_HASH_LENGTH*4));
  assert.equal(r.kind,'not-found');
  assert.ok(Array.from(r.path).length<=MAX_DISPLAY_LENGTH+1);
  assert.equal(parseHash('#'+'/a'.repeat(MAX_HASH_LENGTH)).kind,'not-found');
  const long=parseHash('#/'+'b'.repeat(200));
  assert.equal(long.kind,'not-found');
  assert.ok(Array.from(long.path).length<=MAX_DISPLAY_LENGTH+1);
});

test('malformed percent-encoding does not throw',()=>{
  for(const h of ['#/%E0%A4%A','#/%','#/settings/%zz','#/%C0%AF']){
    let r;assert.doesNotThrow(()=>{r=parseHash(h);},h);
    assert.equal(r.kind,'not-found',h);
  }
});

test('script-looking and control characters are shown only as bounded text',()=>{
  const r=parseHash('#/%3Cscript%3Ealert(1)%3C%2Fscript%3E');
  assert.equal(r.kind,'not-found');
  assert.ok(r.path.length<=MAX_DISPLAY_LENGTH+1);
  const c=parseHash('#/a%00b%0Ac%1Bd%E2%80%AEe');
  assert.equal(c.kind,'not-found');
  assert.doesNotMatch(c.path,/[\u0000-\u001f\u007f-\u009f‮]/);
  assert.equal(parseHash('#/<img src=x onerror=alert(1)>').kind,'not-found');
});

test('non-string input is treated as empty',()=>{
  assert.deepEqual(parseHash(undefined),{kind:'view',view:'library'});
});

test('settings index lists only implemented views and no unavailable section',()=>{
  assert.ok(SETTINGS_SECTIONS.length>0);
  const seen=new Set();
  for(const s of SETTINGS_SECTIONS){
    assert.ok(VIEW_IDS.includes(s.view),s.view);
    assert.ok(VIEW_PATHS[s.view].startsWith('/settings/'),s.view);
    assert.equal(seen.has(s.view),false);seen.add(s.view);
  }
  assert.deepEqual(UNAVAILABLE_SECTIONS,['Import lists','Metadata','Connect (notifications)','UI settings','Backup','Updates','Logs']);
  const labels=SETTINGS_SECTIONS.map(s=>s.label.toLowerCase());
  for(const name of UNAVAILABLE_SECTIONS) assert.equal(labels.includes(name.toLowerCase()),false,name);
  for(const bad of ['importlists','metadata','connect','ui','backup','updates','logs']) assert.equal(parseHash(`#/settings/${bad}`).kind,'not-found',bad);
});

test('components render path as text and link real anchors',()=>{
  const nf=readFileSync(new URL('../src/lib/NotFound.svelte',import.meta.url),'utf8');
  const si=readFileSync(new URL('../src/lib/SettingsIndex.svelte',import.meta.url),'utf8');
  assert.doesNotMatch(nf+si,/\{@html/);
  assert.match(si,/<a href=\{hashFor\(section\.view\)\}>/);
  assert.match(si,/aria-label="Settings sections"/);
});
