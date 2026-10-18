import test from 'node:test';
import assert from 'node:assert/strict';
import {clientAttemptLabel,eligibleDownloadClient,indexerOptions,indexerScope,isDownloadClient,isIndexer,matchesPendingProvider,protocolOf} from '../src/lib/provider-draft.ts';
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
// Explicit choices override default indexer bindings; scoped eligibility still applies.
test('explicit download choices require an enabled client in the requested scope',()=>{
 const client=(id,tv=true,movies=true,enabled=true)=>({id,enabled,settings:{implementation:'qbittorrent',tv:tv?{category:'tv'}:null,movies:movies?{category:'movies'}:null}});
 for(const domain of ['tv','movies']){
  assert.equal(eligibleDownloadClient(client('chosen'),domain),true);
  for(const invalid of [null,client('disabled',true,true,false),client('other-scope',domain!=='tv',domain!=='movies'),{enabled:true,settings:{implementation:'torznab',tv:{},movies:{}}}])assert.equal(eligibleDownloadClient(invalid,domain),false);
 }
 // Unknown persists after reconciliation stops, so its label must not promise an active check.
 assert.equal(clientAttemptLabel('unknown'),'Outcome uncertain');
 assert.equal(clientAttemptLabel('definitely_not_submitted'),'Not submitted');
 assert.equal(clientAttemptLabel('future-state'),'future-state');
});

// SABnzbd is a usenet client: it is never an indexer and only pairs with Newznab, while qBittorrent only pairs with Torznab.
test('download client eligibility follows the indexer transfer protocol in both media scopes',()=>{
 const provider=(implementation,tv={category:'tv'},movies={category:'movies'})=>({id:implementation,enabled:true,settings:{implementation,tv,movies}});
 const sab=provider('sabnzbd'),qbit=provider('qbittorrent'),newznab=provider('newznab',{categories:[5000]},{categories:[2000]}),torznab=provider('torznab',{categories:[5000]},{categories:[2000]});
 assert.deepEqual(['torznab','newznab','qbittorrent','sabnzbd'].map(protocolOf),['torrent','usenet','torrent','usenet']);
 for(const domain of ['tv','movies']){
  assert.equal(eligibleDownloadClient(sab,domain),true);
  assert.equal(eligibleDownloadClient(sab,domain,newznab),true);
  assert.equal(eligibleDownloadClient(sab,domain,'newznab'),true);
  assert.equal(eligibleDownloadClient(sab,domain,torznab),false);
  assert.equal(eligibleDownloadClient(qbit,domain,newznab),false);
  assert.equal(eligibleDownloadClient(qbit,domain,'torznab'),true);
  assert.equal(eligibleDownloadClient(newznab,domain),false);
  assert.equal(indexerScope(sab,domain),null);
  assert.deepEqual(indexerScope(newznab,domain),newznab.settings[domain]);
 }
 assert.equal(eligibleDownloadClient(provider('sabnzbd',null),'tv',newznab),false);
 assert.deepEqual([sab,qbit,newznab,torznab].map(p=>[isIndexer(p),isDownloadClient(p)]),[[false,true],[false,true],[true,false],[true,false]]);
});
