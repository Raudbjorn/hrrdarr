import test from 'node:test';
import assert from 'node:assert/strict';
import {clientMatchesIndexer,eligibleDownloadClient,indexerOptions,matchesPendingProvider} from '../src/lib/provider-draft.ts';
test('unknown creation adoption compares all submitted public settings without equating unrelated records',()=>{
 const pending={name:'shared',enabled:true,priority:3,settings:{implementation:'qbittorrent',endpoint:'http://127.0.0.1:1234',tv:{category:'tv'},movies:{category:'movies'}}};
 const stored={id:'new',revision:1,has_credentials:true,...pending,settings:{movies:{category:'movies'},tv:{category:'tv'},endpoint:pending.settings.endpoint,implementation:'qbittorrent'}};
 assert.equal(matchesPendingProvider(stored,pending),true);
 for(const change of [{name:'other'},{enabled:false},{priority:4},{settings:{...pending.settings,endpoint:'http://127.0.0.1:4321'}},{settings:{...pending.settings,movies:null}}])assert.equal(matchesPendingProvider({...stored,...change},pending),false);
 // An identical concurrent/preexisting config is only an adoption candidate, not creation attribution.
 assert.equal(matchesPendingProvider({...stored,id:'preexisting',revision:8},pending),true);
});

test('indexer draft retains false policies and bindings independently for each media scope',()=>{
 const tv={enable_rss:false,enable_automatic_search:true,enable_interactive_search:false,download_client_id:'tv-client',categories:[5000],anime_categories:[5070],anime_standard_format_search:true};
 const movies={enable_rss:true,enable_automatic_search:false,enable_interactive_search:true,download_client_id:'movie-client',categories:[2000],remove_year:true};
 for(const scope of [tv,movies]){
  assert.deepEqual({...scope,...indexerOptions(scope)},scope);
  assert.deepEqual({...scope,...indexerOptions(scope),download_client_id:null},{...scope,download_client_id:null});
 }
 assert.deepEqual(indexerOptions(null),{enable_rss:true,enable_automatic_search:true,enable_interactive_search:true,download_client_id:null});
});
test('download selection enforces enabled same-domain binding and fails closed without a matching client',()=>{
 const client=(id,tv=true,movies=true,enabled=true)=>({id,enabled,settings:{implementation:'qbittorrent',tv:tv?{category:'tv'}:null,movies:movies?{category:'movies'}:null}});
 const indexer={enabled:true,settings:{implementation:'torznab',tv:{download_client_id:'tv-client'},movies:{download_client_id:'movie-client'}}};
 for(const [domain,id] of [['tv','tv-client'],['movies','movie-client']]){
  assert.equal(clientMatchesIndexer(indexer,client(id),domain),true);
  for(const invalid of [null,client('other'),client(id,true,true,false),client(id,domain!=='tv',domain!=='movies'),{...indexer,id}]){
   assert.equal(clientMatchesIndexer(indexer,invalid,domain),false);
  }
  const unbound={...indexer,settings:{...indexer.settings,[domain]:{download_client_id:null}}};
  assert.equal(clientMatchesIndexer(unbound,client('chosen'),domain),true);
  assert.equal(clientMatchesIndexer({...indexer,enabled:false},client(id),domain),false);
  assert.equal(clientMatchesIndexer({...indexer,settings:{...indexer.settings,[domain]:null}},client(id),domain),false);
 }
 assert.equal(eligibleDownloadClient(indexer,'tv'),false);
 assert.equal(clientMatchesIndexer(null,client('chosen'),'tv'),false);
});
