import test from 'node:test';import assert from 'node:assert/strict';
import {draftFrom,draftError,draftInput,integer,moveProfile} from '../src/lib/delay-profile-draft.ts';
import {listDelayProfiles,getDelayProfileSchema,createDelayProfile,updateDelayProfile,deleteDelayProfile,reorderDelayProfiles,getRevisionPolicy,updateRevisionPolicy} from '../src/lib/api.ts';
const defaults={settings:{torrent_delay_minutes:0,usenet_delay_minutes:0,enable_torrent:true,enable_usenet:true,preferred_protocol:'usenet',bypass_if_highest_quality:false,bypass_if_above_custom_format_score:false,minimum_custom_format_score:0},tag_ids:[]};
test('legacy configuration preserves known minutes without inventing a read model; signed bounds and independent preference',()=>{
 const legacy={id:9,media_type:'tv',is_global:true,position:0,tag_ids:[],settings:{semantics:'legacy_age_only',torrent_delay_minutes:41,usenet_delay_minutes:null}};
 const draft=draftFrom(defaults,legacy);assert.equal(draft.torrent,'41');assert.equal(draft.usenet,'0');assert.equal(legacy.settings.usenet_delay_minutes,null);assert.equal(legacy.settings.enable_usenet,undefined);
 draft.enable_usenet=false;draft.preferred_protocol='usenet';draft.score='-2147483648';assert.equal(draftError(draft,true,[],[],9),'');assert.equal(draftInput(draft).settings.preferred_protocol,'usenet');
 for(const value of ['','1.2','2e2','NaN','Infinity',' 1','-1','10081'])assert.equal(integer(value,0,10080),null);
 draft.score='2147483648';assert.ok(draftError(draft,true,[],[],9));draft.score='0';draft.enable_torrent=false;assert.ok(draftError(draft,true,[],[],9));
});
test('tag graph rejects unknown and reused IDs; full order preserves identity',()=>{
 const draft=draftFrom(defaults),tags=[{id:1,media_type:'tv',label:'one'}];assert.ok(draftError(draft,false,tags,[],null));draft.tag_ids=[1];assert.equal(draftError(draft,false,tags,[],null),'');assert.ok(draftError(draft,false,tags,[{id:2,tag_ids:[1]}],null));assert.equal(draftError(draft,false,tags,[{id:2,tag_ids:[1]}],2),'');draft.tag_ids=[2];assert.ok(draftError(draft,false,tags,[],null));assert.deepEqual(moveProfile([5,8,9],8,-1),[8,5,9]);assert.deepEqual(moveProfile([5,8,9],5,-1),[5,8,9]);
});
test('native mutations return catalog JSON including DELETE and scope revisions',async()=>{
 const original=globalThis.fetch,calls=[],catalog={revision:2,configured:true,availability_delay_days:0,profiles:[]};globalThis.fetch=async(url,options)=>{calls.push({url,options});return Response.json(catalog);};
 try{for(const domain of ['tv','movies']){
  await listDelayProfiles(domain);assert.equal(calls.at(-1).url,`/api/v1/${domain}/delay-profiles`);await getDelayProfileSchema(domain);assert.ok(calls.at(-1).url.endsWith('/schema'));
  await createDelayProfile(domain,{revision:1,profile:defaults});assert.deepEqual(JSON.parse(calls.at(-1).options.body),{revision:1,profile:defaults});await updateDelayProfile(domain,9,{revision:1,profile:defaults});assert.ok(calls.at(-1).url.endsWith('/9'));
  assert.deepEqual(await deleteDelayProfile(domain,9,1),{ok:true,data:catalog});assert.ok(calls.at(-1).url.endsWith('/9?revision=1'));assert.equal(calls.at(-1).options.method,'DELETE');
  await reorderDelayProfiles(domain,{revision:1,ids:[4,3]});assert.deepEqual(JSON.parse(calls.at(-1).options.body),{revision:1,ids:[4,3]});await getRevisionPolicy(domain);assert.equal(calls.at(-1).url,`/api/v1/${domain}/revision-policy`);await updateRevisionPolicy(domain,{revision:3,mode:'do_not_prefer'});assert.deepEqual(JSON.parse(calls.at(-1).options.body),{revision:3,mode:'do_not_prefer'});
 }}finally{globalThis.fetch=original;}
});
