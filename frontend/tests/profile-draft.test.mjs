import test from 'node:test';
import assert from 'node:assert/strict';
import {fromInput,toInput,reorder,addGroup,moveLeaf,dissolve,setAllowed,validateDraft} from '../src/lib/profile-draft.ts';
const leaf = id => ({kind:'quality',quality_id:id,allowed:true,min_size:null,max_size:null,preferred_size:null});
const policy = {upgrade_allowed:true,cutoff:{kind:'group',position:1},min_format_score:0,cutoff_format_score:0,min_upgrade_format_score:1,language_id:null,format_items:[]};
const input = () => ({name:'HD',items:[leaf(1),{kind:'group',name:'Web',allowed:true,items:[leaf(3),leaf(5)]}],policy});
test('group cutoff follows root reordering and dissolution demands an explicit new choice',()=>{
 const d=fromInput(input()); d.roots=reorder(d.roots,1,-1);
 assert.deepEqual(toInput(d).policy.cutoff,{kind:'group',position:0});
 dissolve(d,0); assert.equal(d.cutoff,''); assert.match(validateDraft(d,'tv'),/cutoff/);
 assert.deepEqual(d.roots.map(r=>r.item.quality_id),[3,5,1]);
});
test('moving cutoff quality into a group invalidates cutoff without silently selecting a different preference',()=>{
 const data=input(); data.policy={...policy,cutoff:{kind:'quality',quality_id:1}};
 const d=fromInput(data);moveLeaf(d,0,null,d.roots[1].key);
 assert.equal(d.cutoff,'');assert.deepEqual(d.roots[0].item.items.map(l=>l.quality_id),[3,5,1]);
});
test('group toggles propagate and moving children out preserves each leaf exactly once',()=>{
 const d=fromInput(input());setAllowed(d.roots[1],false);
 assert.ok(d.roots[1].item.items.every(l=>!l.allowed));
 moveLeaf(d,1,0,'root'); assert.equal(d.roots[2].item.quality_id,3);
 assert.deepEqual(d.roots[1].item.items.map(l=>l.quality_id),[5]);
 addGroup(d,'New'); assert.match(validateDraft(d,'tv'),/at least one quality/);
});
test('sparse legacy policy and null sizes roundtrip without invented defaults',()=>{
 const data={name:'Legacy',items:[leaf(1)],policy:null};
 assert.deepEqual(toInput(fromInput(data)),data);
 const d=fromInput(data);d.roots[0].item.max_size=1500;
 assert.match(validateDraft(d,'tv'),/range/); assert.equal(validateDraft(d,'movies'),null);
 d.roots[0].item.min_size=1600;assert.match(validateDraft(d,'movies'),/minimum/);
});
test('structural edits preserve effective disabled leaves behind a disabled parent',()=>{
 const data=input();data.items[1].allowed=false;
 const d=fromInput(data);dissolve(d,1);assert.deepEqual(d.roots.map(r=>r.item.allowed),[true,false,false]);
 const moved=fromInput(data);moveLeaf(moved,1,0,'root');assert.equal(moved.roots[1].item.allowed,false);assert.equal(moved.roots[2].item.allowed,false);
 const into=fromInput(data);moveLeaf(into,0,null,into.roots[1].key);assert.deepEqual(into.roots[0].item.items.map(l=>l.allowed),[false,false,true]);
});
