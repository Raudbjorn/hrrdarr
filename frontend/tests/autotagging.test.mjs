import test from 'node:test';
import assert from 'node:assert/strict';
import {parseTextList,parseIntList,newSpec,initFields,emptyRule,toInput,draftFromRule,summarizeCondition,summarizeSpecification} from '../src/lib/autotag-draft.ts';
const f=(name,kind,required=true,options=[])=>({name,label:name,kind,required,options});
const schema=[
 {implementation:'genre',label:'Genre',fields:[f('values','text_list')]},
 {implementation:'year',label:'Year',fields:[f('min','integer',false),f('max','integer',false)]},
 {implementation:'root_folder',label:'Root folder',fields:[f('path','text')]},
 {implementation:'collection',label:'Collection',fields:[f('values','integer_list')]},
 {implementation:'series_type',label:'Series type',fields:[f('values','enum_list',true,['standard','anime'])]},
 {implementation:'monitored',label:'Monitored',fields:[f('value','boolean')]},
];
const catalog=[{id:1,media_type:'tv',label:'a'},{id:2,media_type:'tv',label:'b'}];
const entry=name=>schema.find(e=>e.implementation===name);
const draft=(...specs)=>({...emptyRule(),name:' Rule ',tagIds:[1,1,2],specs});
test('list parsers split commas and newlines',()=>{
 assert.deepEqual(parseTextList(' a, b\n\nc ,, '),['a','b','c']);
 assert.deepEqual(parseIntList('1, 2\n3'),[1,2,3]);
 for(const bad of ['1.5','-1','x','9007199254740993'])assert.equal(parseIntList(bad),null);
});
test('valid drafts convert to the documented condition shapes',()=>{
 const g=newSpec(entry('genre'));g.text.values='Drama, Anime';
 const y=newSpec(entry('year'));y.text.min='2000';y.required=true;
 const r=newSpec(entry('root_folder'));r.text.path=' /x ';r.negate=true;
 const c=newSpec(entry('collection'));c.text.values='5,6';
 const s=newSpec(entry('series_type'));s.pick.values=['anime'];
 const m=newSpec(entry('monitored'));m.flag.value=false;
 const res=toInput(draft(g,y,r,c,s,m),schema,catalog);
 assert.ok(res.ok,res.error);
 assert.equal(res.input.name,'Rule');assert.deepEqual(res.input.tag_ids,[1,2]);
 assert.deepEqual(res.input.specifications.map(x=>x.condition),[
  {implementation:'genre',values:['Drama','Anime']},{implementation:'year',min:2000,max:null},{implementation:'root_folder',path:'/x'},
  {implementation:'collection',values:[5,6]},{implementation:'series_type',values:['anime']},{implementation:'monitored',value:false}]);
 assert.equal(res.input.specifications[1].required,true);assert.equal(res.input.specifications[2].negate,true);
});
test('validation mirrors backend bounds',()=>{
 const ok=()=>{const g=newSpec(entry('genre'));g.text.values='Drama';return g;};
 const bad=(d)=>assert.equal(toInput(d,schema,catalog).ok,false);
 bad({...draft(ok()),name:''});bad({...draft(ok()),name:'x'.repeat(257)});bad({...draft(ok()),name:'é'.repeat(129)});
 bad({...draft(ok()),tagIds:[]});bad({...draft(ok()),tagIds:[9]});
 bad({...draft(ok()),tagIds:Array.from({length:65},(_, i)=>i+1)});
 bad(draft());
 bad(draft(...Array.from({length:65},ok)));
 const empty=newSpec(entry('genre'));bad(draft(empty));
 const many=newSpec(entry('genre'));many.text.values=Array.from({length:65},(_, i)=>`v${i}`).join(',');bad(draft(many));
 const noRange=newSpec(entry('year'));bad(draft(noRange));
 const inverted=newSpec(entry('year'));inverted.text.min='2010';inverted.text.max='2000';bad(draft(inverted));
 const huge=newSpec(entry('year'));huge.text.min='10000';bad(draft(huge));
 const rel=newSpec(entry('root_folder'));rel.text.path='x';bad(draft(rel));
 const noPick=newSpec(entry('series_type'));bad(draft(noPick));
 const nameless=ok();nameless.name='';bad(draft(nameless));
 const unknown=ok();unknown.implementation='nope';bad(draft(unknown));
 const eq=newSpec(entry('year'));eq.text.min='2000';eq.text.max='2000';assert.ok(toInput(draft(eq),schema,catalog).ok);
});
test('rules round-trip through drafts and summaries',()=>{
 const rule={id:3,media_type:'tv',name:'R',remove_tags_automatically:true,tag_ids:[2],specifications:[
  {id:1,name:'G',negate:false,required:true,condition:{implementation:'genre',values:['Drama']}},
  {id:2,name:'Y',negate:true,required:false,condition:{implementation:'year',min:2000,max:2010}}]};
 const d=draftFromRule(rule,schema);assert.equal(d.remove,true);
 const res=toInput(d,schema,catalog);assert.ok(res.ok);
 assert.deepEqual(res.input.specifications.map(s=>s.condition),rule.specifications.map(s=>s.condition));
 assert.equal(summarizeCondition(rule.specifications[0].condition),'genre is Drama');
 assert.equal(summarizeCondition(rule.specifications[1].condition),'year 2000–2010');
 assert.equal(summarizeCondition({implementation:'year',min:null,max:5}),'year at most 5');
 assert.equal(summarizeCondition({implementation:'monitored',value:true}),'monitored is yes');
 assert.equal(summarizeSpecification(rule.specifications[1]),'Y: not year 2000–2010');
 initFields(d.specs[0],entry('monitored'));assert.deepEqual(d.specs[0].flag,{value:true});
});
