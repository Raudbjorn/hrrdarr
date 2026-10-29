import test from 'node:test';
import assert from 'node:assert/strict';
import {clientAttemptLabel,eligibleDownloadClient,indexerOptions,indexerScope,isClientImplementation,isDownloadClient,isIndexer,matchesPendingProvider,normalizeUrlBase,parseTransmissionEndpoint,protocolOf,transmissionEndpoint,transmissionPlacementConflict,transmissionPlacementProblem,transmissionRpcUrl,validTransmissionCategory,validTransmissionDirectory} from '../src/lib/provider-draft.ts';
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

test('Transmission is a torrent download client for both domains',()=>{
 assert.equal(protocolOf('transmission'),'torrent');
 assert.equal(isClientImplementation('transmission'),true);
 for(const domain of ['tv','movies']){
  const client={id:'tx',enabled:true,settings:{implementation:'transmission',endpoint:'http://127.0.0.1:1/transmission',tv:domain==='tv'?{placement:{strategy:'category',category:'tv'}}:null,movies:domain==='movies'?{placement:{strategy:'directory',directory:'/srv/movies'}}:null}};
  assert.equal(isDownloadClient(client),true);
  assert.equal(eligibleDownloadClient(client,domain),true);
  assert.equal(eligibleDownloadClient(client,domain==='tv'?'movies':'tv'),false);
 }
});
test('Transmission endpoint composes from host, port, SSL and URL base and parses back',()=>{
 const cases=[
  [{host:'transmission.example',port:9091,useSsl:false,urlBase:'/transmission'},'http://transmission.example:9091/transmission'],
  [{host:' 10.0.0.5 ',port:443,useSsl:true,urlBase:'proxy/tx/'},'https://10.0.0.5:443/proxy/tx'],
  [{host:'[::1]',port:19091,useSsl:false,urlBase:''},'http://[::1]:19091'],
 ];
 for(const [address,endpoint] of cases){
  assert.equal(transmissionEndpoint(address),endpoint);
  const parsed=parseTransmissionEndpoint(endpoint);
  assert.equal(transmissionEndpoint(parsed),endpoint,'round trip is stable');
 }
 assert.equal(transmissionRpcUrl('http://h:1/transmission/'),'http://h:1/transmission/rpc');
 // Default ports are implicit in a URL; parsing restores them so the editor always shows a port.
 assert.deepEqual(parseTransmissionEndpoint('https://h/x'),{host:'h',port:443,useSsl:true,urlBase:'/x'});
 for(const bad of [{host:'',port:9091},{host:'bad host',port:9091},{host:'h',port:0},{host:'h',port:65536},{host:'h',port:1.5},{host:'h',port:9091,urlBase:'/a/../b'},{host:'h',port:9091,urlBase:'/a?x=1'}])
  assert.equal(transmissionEndpoint({useSsl:false,urlBase:'',...bad}),null,JSON.stringify(bad));
 for(const bad of ['ftp://h/x','http://u:p@h/x','http://h/x?y','http://h/x#f','not a url'])assert.equal(parseTransmissionEndpoint(bad),null,bad);
 assert.equal(normalizeUrlBase('///'),'');
});
test('Transmission placements mirror the server rules and keep domains apart',()=>{
 for(const ok of ['tv','tv-sonarr','Movies_4K','a.b'])assert.equal(validTransmissionCategory(ok),true,ok);
 for(const bad of ['','.','..','tv/sub','tv,hd','x'.repeat(65),'tv hd'])assert.equal(validTransmissionCategory(bad),false,bad);
 for(const ok of ['/srv/tv','C:\\Downloads\\tv','D:/movies'])assert.equal(validTransmissionDirectory(ok),true,ok);
 for(const bad of ['relative/tv','/','/srv/','/srv/../etc','/srv/./tv','C:','/srv/a:b','/srv/\u0007','/srv/\u0085'])assert.equal(validTransmissionDirectory(bad),false,JSON.stringify(bad));
 assert.match(transmissionPlacementProblem({strategy:'category',category:'tv/x'}),/without slashes/);
 assert.match(transmissionPlacementProblem({strategy:'directory',directory:'tv'}),/absolute/);
 assert.equal(transmissionPlacementProblem({strategy:'directory',directory:'/srv/tv'}),null);
 const cat=category=>({strategy:'category',category}),dir=directory=>({strategy:'directory',directory});
 assert.match(transmissionPlacementConflict(cat('Media'),cat('media')),/categories must differ/);
 for(const [tv,movies] of [['/srv/media','/srv/media/movies'],['/SRV/media/tv','/srv/media'],['/srv/x','/srv/x']])assert.match(transmissionPlacementConflict(dir(tv),dir(movies)),/nested/);
 // Sibling prefixes are not nesting; mixed strategies are kept apart by label/subfolder versus directory.
 assert.equal(transmissionPlacementConflict(dir('/srv/media'),dir('/srv/media-movies')),null);
 assert.equal(transmissionPlacementConflict(cat('tv'),dir('/srv/movies')),null);
});

test('NZBGet is a usenet download client for both domains and never accepts torrents',()=>{
 assert.equal(protocolOf('nzbget'),'usenet');
 assert.equal(isClientImplementation('nzbget'),true);
 const newznab={id:'nz',enabled:true,settings:{implementation:'newznab',endpoint:'http://127.0.0.1:1',tv:{categories:[5030]},movies:null}};
 for(const domain of ['tv','movies']){
  const client={id:'ng',enabled:true,settings:{implementation:'nzbget',endpoint:'http://127.0.0.1:6789',tv:domain==='tv'?{category:'tv'}:null,movies:domain==='movies'?{category:'movies'}:null}};
  assert.equal(isDownloadClient(client),true);
  assert.equal(eligibleDownloadClient(client,domain),true);
  assert.equal(eligibleDownloadClient(client,domain==='tv'?'movies':'tv'),false);
 }
});

test('Deluge is a torrent download client for both domains and is never bound to a usenet indexer',()=>{
 assert.equal(protocolOf('deluge'),'torrent');
 assert.equal(isClientImplementation('deluge'),true);
 for(const domain of ['tv','movies']){
  const client={id:'dg',enabled:true,settings:{implementation:'deluge',endpoint:'http://127.0.0.1:8112',tv:domain==='tv'?{category:'tv'}:null,movies:domain==='movies'?{category:'movies'}:null}};
  assert.equal(isDownloadClient(client),true);
  assert.equal(eligibleDownloadClient(client,domain),true);
  assert.equal(eligibleDownloadClient(client,domain==='tv'?'movies':'tv'),false);
  assert.equal(eligibleDownloadClient(client,domain,'newznab'),false);
  assert.equal(eligibleDownloadClient(client,domain,'torznab'),true);
 }
});

test('rTorrent is a torrent download client for both domains',()=>{
 assert.equal(protocolOf('rtorrent'),'torrent');
 assert.equal(isClientImplementation('rtorrent'),true);
 const client={id:'rt',enabled:true,settings:{implementation:'rtorrent',endpoint:'http://127.0.0.1/RPC2',tv:{category:'tv'},movies:null}};
 assert.equal(isDownloadClient(client),true);
 assert.equal(eligibleDownloadClient(client,'tv','torznab'),true);
 assert.equal(eligibleDownloadClient(client,'tv','newznab'),false);
});

test('uTorrent is a torrent download client for both domains',()=>{
 assert.equal(protocolOf('utorrent'),'torrent');
 assert.equal(isClientImplementation('utorrent'),true);
 const client={id:'ut',enabled:true,settings:{implementation:'utorrent',endpoint:'http://127.0.0.1:8080/gui',tv:null,movies:{category:'movies'}}};
 assert.equal(isDownloadClient(client),true);
 assert.equal(eligibleDownloadClient(client,'movies','torznab'),true);
 assert.equal(eligibleDownloadClient(client,'movies','newznab'),false);
});

test('Aria2 is a torrent download client for both domains',()=>{
 assert.equal(protocolOf('aria2'),'torrent');
 assert.equal(isClientImplementation('aria2'),true);
 const client={id:'a2',enabled:true,settings:{implementation:'aria2',endpoint:'http://127.0.0.1:6800/jsonrpc',tv:null,movies:{category:'movies'}}};
 assert.equal(isDownloadClient(client),true);
 assert.equal(eligibleDownloadClient(client,'movies','torznab'),true);
 assert.equal(eligibleDownloadClient(client,'movies','newznab'),false);
});

test('Flood is a torrent download client for both domains',()=>{
 assert.equal(protocolOf('flood'),'torrent');
 assert.equal(isClientImplementation('flood'),true);
 const client={id:'fl',enabled:true,settings:{implementation:'flood',endpoint:'http://127.0.0.1:3000',tv:null,movies:{category:'movies'}}};
 assert.equal(isDownloadClient(client),true);
 assert.equal(eligibleDownloadClient(client,'movies','torznab'),true);
 assert.equal(eligibleDownloadClient(client,'movies','newznab'),false);
});

test('Download Station is a torrent download client for both domains',()=>{
 assert.equal(protocolOf('downloadstation'),'torrent');
 assert.equal(isClientImplementation('downloadstation'),true);
 const client={id:'ds',enabled:true,settings:{implementation:'downloadstation',endpoint:'https://nas.example:5001',tv:null,movies:{category:'movies'}}};
 assert.equal(isDownloadClient(client),true);
 assert.equal(eligibleDownloadClient(client,'movies','torznab'),true);
 assert.equal(eligibleDownloadClient(client,'movies','newznab'),false);
});
