import type {IndexerParameter,MediaDomain,MovieIndexerScope,Provider,ProviderCredentials,ProviderImplementation,ProviderInput,TorrentPlacement,TrackerScope,TvIndexerScope} from './api.generated';
export type PendingProvider = Omit<ProviderInput,'credentials'>;
// Compare structured public settings regardless of JSON object key order; arrays retain order.
function canonical(value:unknown):unknown {
  if(Array.isArray(value))return value.map(canonical);
  if(value && typeof value==='object')return Object.fromEntries(Object.entries(value).sort(([a],[b])=>a.localeCompare(b)).map(([key,item])=>[key,canonical(item)]));
  return value;
}
export function matchesPendingProvider(provider:Provider,pending:PendingProvider):boolean {
  return provider.name===pending.name && provider.enabled===pending.enabled && provider.priority===pending.priority && JSON.stringify(canonical(provider.settings))===JSON.stringify(canonical(pending.settings));
}

// Transfer protocol per implementation; mirrors the server's provider_implementations registry.
// The server remains authoritative: a mismatched grab or binding is rejected with protocol_mismatch.
export type TransferProtocol='torrent'|'usenet';
const protocols:Record<ProviderImplementation,TransferProtocol>={torznab:'torrent',torrentrss:'torrent',qbittorrent:'torrent',transmission:'torrent',deluge:'torrent',rtorrent:'torrent',utorrent:'torrent',aria2:'torrent',flood:'torrent',downloadstation:'torrent',newznab:'usenet',sabnzbd:'usenet',nzbget:'usenet',fanzub:'usenet',nyaa:'torrent',iptorrents:'torrent',torrentleech:'torrent',broadcasthenet:'torrent',filelist:'torrent',hdbits:'torrent',passthepopcorn:'torrent',torrentpotato:'torrent'};
export const protocolOf=(implementation:ProviderImplementation):TransferProtocol=>protocols[implementation];
export type TrackerImplementation='nyaa'|'fanzub'|'iptorrents'|'torrentleech'|'broadcasthenet'|'filelist'|'hdbits'|'passthepopcorn'|'torrentpotato';
export type IndexerImplementation='torznab'|'newznab'|'torrentrss'|TrackerImplementation;
// Credential shape per tracker: nothing, a write-only feed bundle (cookie and query parameters), one API key,
// or an account pair (user name plus passkey / API key).
export type TrackerCredentialKind='none'|'feed'|'api_key'|'account';
export type TrackerInfo={label:string;tv:boolean;movies:boolean;searchable:boolean;categorised:boolean;numericParameterNames?:boolean;credential:TrackerCredentialKind;secretLabels:[string,string];hint:string;note:string};
// Mirrors the server's TrackerKind table; the server stays authoritative and rejects any mismatch.
export const TRACKERS:Record<TrackerImplementation,TrackerInfo>={
 nyaa:{label:'Nyaa',tv:true,movies:true,searchable:true,categorised:false,credential:'feed',secretLabels:['',''],hint:'https://nyaa.si/',note:'RSS feed and text search. Optional feed parameters (for example c=1_2, f=0) override the defaults c=1_0 and f=1.'},
 fanzub:{label:'Fanzub (Usenet)',tv:true,movies:false,searchable:true,categorised:false,credential:'feed',secretLabels:['',''],hint:'https://fanzub.com/rss',note:'Anime NZB feed and text search; pairs only with SABnzbd or NZBGet. TV only.'},
 iptorrents:{label:'IPTorrents',tv:true,movies:true,searchable:false,categorised:false,numericParameterNames:true,credential:'feed',secretLabels:['',''],hint:'https://iptorrents.com/t.rss',note:'RSS only. Put the account id (u), passkey (tp) and category numbers from your generated feed URL into the write-only feed parameters, one name=value per line (a bare category number is name=).'},
 torrentleech:{label:'Torrentleech',tv:true,movies:false,searchable:false,categorised:false,credential:'api_key',secretLabels:['RSS key',''],hint:'https://rss.torrentleech.org',note:'RSS only. The RSS key is appended to the address as a path segment. TV only.'},
 broadcasthenet:{label:'BroadcastheNet',tv:true,movies:false,searchable:true,categorised:false,credential:'api_key',secretLabels:['API key',''],hint:'https://api.broadcasthe.net/',note:'JSON-RPC API with TVDB/TVRage/name search. TV only.'},
 filelist:{label:'FileList',tv:true,movies:true,searchable:true,categorised:true,credential:'account',secretLabels:['Username','Passkey'],hint:'https://filelist.io/api.php',note:'JSON API with IMDb or name search. Categories are FileList category ids.'},
 hdbits:{label:'HDBits',tv:true,movies:true,searchable:true,categorised:true,credential:'account',secretLabels:['Username','Passkey'],hint:'https://hdbits.org/api/torrents',note:'JSON API with IMDb, TVDB or text search. Categories are HDBits category ids (1 movie, 2 TV).'},
 passthepopcorn:{label:'PassThePopcorn',tv:false,movies:true,searchable:true,categorised:false,credential:'account',secretLabels:['API user','API key'],hint:'https://passthepopcorn.me/torrents.php',note:'JSON API with IMDb or title search. Movies only.'},
 torrentpotato:{label:'TorrentPotato',tv:false,movies:true,searchable:true,categorised:false,credential:'account',secretLabels:['User','Passkey'],hint:'https://tracker.example/potato',note:'TorrentPotato JSON API. Movies only.'},
};
export const isTrackerImplementation=(implementation:ProviderImplementation):implementation is TrackerImplementation=>Object.hasOwn(TRACKERS,implementation);
export const isIndexerImplementation=(implementation:ProviderImplementation):implementation is IndexerImplementation=>implementation==='torznab'||implementation==='newznab'||implementation==='torrentrss'||isTrackerImplementation(implementation);
export const isClientImplementation=(implementation:ProviderImplementation):implementation is 'qbittorrent'|'sabnzbd'|'nzbget'|'transmission'|'deluge'|'rtorrent'|'utorrent'|'aria2'|'flood'|'downloadstation'=>implementation==='qbittorrent'||implementation==='sabnzbd'||implementation==='nzbget'||implementation==='transmission'||implementation==='deluge'||implementation==='rtorrent'||implementation==='utorrent'||implementation==='aria2'||implementation==='flood'||implementation==='downloadstation';
export const isIndexer=(provider:Provider)=>isIndexerImplementation(provider.settings.implementation);
export const isDownloadClient=(provider:Provider)=>isClientImplementation(provider.settings.implementation);
// The operation policy every indexer kind shares; feeds and trackers never bind a download client.
export type IndexerScopeView={enable_rss?:boolean;enable_automatic_search?:boolean|null;enable_interactive_search?:boolean|null;download_client_id?:string|null};
export function indexerScope(provider:Provider|null,domain:MediaDomain):IndexerScopeView|null {
  if(!provider)return null;
  const settings=provider.settings;
  if(settings.implementation==='torznab'||settings.implementation==='newznab'){const scope=settings[domain];return scope;}
  if(settings.implementation==='torrentrss'){const scope=settings[domain];return scope?{enable_rss:scope.enable_rss??true,enable_automatic_search:false,enable_interactive_search:false,download_client_id:null}:null;}
  if(isTrackerImplementation(settings.implementation)){
    const scope=(settings as {tv:TrackerScope|null;movies:TrackerScope|null})[domain];
    return scope?{enable_rss:scope.enable_rss??true,enable_automatic_search:scope.enable_automatic_search??false,enable_interactive_search:scope.enable_interactive_search??false,download_client_id:null}:null;
  }
  return null;
}
export function eligibleInteractiveIndexer(provider:Provider,domain:MediaDomain):boolean {
  const scope=indexerScope(provider,domain);
  return provider.enabled&&isIndexer(provider)&&scope!==null&&scope.enable_interactive_search!==false;
}
export function indexerOptions(scope:TvIndexerScope|MovieIndexerScope|null|undefined) {
  return {enable_rss:scope?.enable_rss??true,enable_automatic_search:scope?.enable_automatic_search??true,enable_interactive_search:scope?.enable_interactive_search??true,download_client_id:scope?.download_client_id??null};
}
// When an indexer (or its implementation) is given, the client must also speak its protocol.
export function eligibleDownloadClient(provider:Provider|null,domain:MediaDomain,indexer?:Provider|ProviderImplementation|null):provider is Provider {
  if(!provider || !provider.enabled || !isDownloadClient(provider) || provider.settings[domain]===null)return false;
  const indexerImplementation=typeof indexer==='string'?indexer:indexer?.settings.implementation;
  return !indexerImplementation || protocolOf(indexerImplementation)===protocolOf(provider.settings.implementation);
}
/** Keep provider records from every loaded page so selections survive pagination. */
export function mergeProviderPages(seen: readonly Provider[], page: readonly Provider[]): Provider[] {
  const byId = new Map(seen.map((provider) => [provider.id, provider]));
  for (const provider of page) byId.set(provider.id, provider);
  return [...byId.values()];
}
const clientAttemptLabels:Record<string,string>={selected:'Client selected',prepared:'Ready to send',dispatching:'Sending',definitely_not_submitted:'Not submitted',observed:'Confirmed in client',unknown:'Outcome uncertain'};
export const clientAttemptLabel=(state:string)=>clientAttemptLabels[state]??state;

// Transmission's saved endpoint is one URL: scheme (SSL), host, port and URL base. The editor shows
// those parts separately; the server validates the composed URL and appends the RPC path `/rpc`.
export type TransmissionAddress={host:string;port:number;useSsl:boolean;urlBase:string};
export const TRANSMISSION_DEFAULT_PORT=9091;
export const TRANSMISSION_DEFAULT_URL_BASE='/transmission';
const MAX_PORT=65535;
const hostPattern=/^(?:\[[0-9A-Fa-f:.]+\]|[A-Za-z0-9](?:[A-Za-z0-9.-]{0,251}[A-Za-z0-9])?)$/;
/** Canonical URL base: empty, or `/segment(/segment)*` without dot segments, query or fragment. */
export function normalizeUrlBase(value:string):string|null{
  const trimmed=value.trim().replace(/\/+$/,'');
  if(!trimmed)return '';
  const path=trimmed.startsWith('/')?trimmed:`/${trimmed}`;
  const parts=path.slice(1).split('/');
  if(parts.some(part=>!part||part==='.'||part==='..'||!/^[A-Za-z0-9._~!$&'()*+,;=:@%-]+$/.test(part)))return null;
  return path;
}
export function transmissionEndpoint(address:TransmissionAddress):string|null{
  const host=address.host.trim(),base=normalizeUrlBase(address.urlBase);
  if(!hostPattern.test(host)||!Number.isInteger(address.port)||address.port<1||address.port>MAX_PORT||base===null)return null;
  return `${address.useSsl?'https':'http'}://${host}:${address.port}${base}`;
}
export function parseTransmissionEndpoint(endpoint:string):TransmissionAddress|null{
  let url:URL;
  try{url=new URL(endpoint);}catch{return null;}
  if((url.protocol!=='http:'&&url.protocol!=='https:')||url.username||url.password||url.search||url.hash||!url.hostname)return null;
  const useSsl=url.protocol==='https:';
  // The saved pathname is already encoded; decoding breaks spaces and malformed escapes on reopen.
  const base=normalizeUrlBase(url.pathname);
  if(base===null)return null;
  return {host:url.hostname,port:url.port?Number(url.port):useSsl?443:80,useSsl,urlBase:base};
}
export const transmissionRpcUrl=(endpoint:string)=>`${endpoint.replace(/\/+$/,'')}/rpc`;
// Mirrors the server's placement rules so the editor can explain a rejection before sending.
export const validTransmissionCategory=(value:string)=>/^[A-Za-z0-9._-]{1,64}$/.test(value)&&value!=='.'&&value!=='..';
export function validTransmissionDirectory(value:string):boolean{
  const drive=/^[A-Za-z]:[\\/]/.test(value);
  if(value.length>1024||!(drive||value.startsWith('/'))||/[\u0000-\u001f\u007f-\u009f]/.test(value))return false;
  const rest=value.slice(drive?3:1);
  return rest.length>0&&!rest.includes(':')&&rest.split(/[\\/]/).every(part=>part&&part!=='.'&&part!=='..');
}
const segments=(path:string)=>path.toLowerCase().split(/[\\/]/).filter(Boolean);
function nests(a:string,b:string){const x=segments(a),y=segments(b);return x.length>=y.length&&y.every((part,i)=>x[i]===part);}
export function transmissionPlacementProblem(placement:TorrentPlacement):string|null{
  return placement.strategy==='category'
    ? (validTransmissionCategory(placement.category)?null:'Categories use letters, digits, dot, dash or underscore (at most 64), without slashes.')
    : (validTransmissionDirectory(placement.directory)?null:'Download directories must be absolute paths on the Transmission host, without a trailing slash or dot segments.');
}
export function transmissionPlacementConflict(tv:TorrentPlacement,movies:TorrentPlacement):string|null{
  if(tv.strategy==='category'&&movies.strategy==='category'&&tv.category.toLowerCase()===movies.category.toLowerCase())return 'TV and movie categories must differ so each domain only sees its own torrents.';
  if(tv.strategy==='directory'&&movies.strategy==='directory'&&(nests(tv.directory,movies.directory)||nests(movies.directory,tv.directory)))return 'TV and movie download directories must not be the same or nested.';
  return null;
}

// ---- Tracker editor drafts -------------------------------------------------------------------------
export type TrackerDraft={rss:boolean;automatic:boolean;interactive:boolean;minimum:string;categories:string};
export function trackerDraft(scope:TrackerScope|null|undefined,info:TrackerInfo,defaults?:TrackerScope):TrackerDraft{
  const base=scope??defaults;
  return {rss:base?.enable_rss??true,automatic:info.searchable&&(base?.enable_automatic_search??true),interactive:info.searchable&&(base?.enable_interactive_search??true),minimum:base?.minimum_seeders==null?'':String(base.minimum_seeders),categories:(base?.categories??[]).join(', ')};
}
/** A scope for the request body, or null with the reason when the draft is not valid. */
export function trackerScope(draft:TrackerDraft,info:TrackerInfo):{scope:TrackerScope}|{problem:string}{
  const minimum=draft.minimum.trim();
  if(minimum&&!/^\d{1,7}$/.test(minimum))return {problem:'Minimum seeders is a whole number from 0 to 1,000,000.'};
  const minimumSeeders=minimum?Number(minimum):null;
  if(minimumSeeders!==null&&minimumSeeders>1_000_000)return {problem:'Minimum seeders is a whole number from 0 to 1,000,000.'};
  let categories:number[]=[];
  if(draft.categories.trim()){
    if(!info.categorised)return {problem:`${info.label} has no category ids.`};
    const parts=draft.categories.split(',').map(part=>part.trim());
    categories=parts.map(Number);
    if(parts.length>64||parts.some(part=>!/^[1-9]\d{0,6}$/.test(part))||new Set(categories).size!==categories.length)return {problem:'Categories are distinct whole numbers, separated by commas.'};
  }
  return {scope:{enable_rss:draft.rss,enable_automatic_search:info.searchable?draft.automatic:false,enable_interactive_search:info.searchable?draft.interactive:false,minimum_seeders:minimumSeeders,categories}};
}
/** `name=value` per line (a bare `48` or `48=` is an empty value); null when a line is not a valid parameter. */
export function parseParameters(text:string,allowNumericNames=false):IndexerParameter[]|null{
  const lines=text.split('\n').map(line=>line.trim()).filter(Boolean);
  if(lines.length>16)return null;
  const parameters:IndexerParameter[]=[];
  for(const line of lines){
    const at=line.indexOf('=');
    const name=at<0?line:line.slice(0,at).trim(),value=at<0?'':line.slice(at+1).trim();
    if(!(allowNumericNames?/^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$/:/^[A-Za-z][A-Za-z0-9_.-]{0,63}$/).test(name)||value.length>1024||/[\u0000-\u001f\u007f]/.test(value)||parameters.some(item=>item.name.toLowerCase()===name.toLowerCase()))return null;
    parameters.push({name,value});
  }
  return parameters;
}
export type TrackerSecrets={apiKey:string;username:string;password:string;cookie:string;tvParameters:string;movieParameters:string};
/** The replacement credential bundle for a tracker, or an explanation when required fields are missing. */
export function trackerCredentials(info:TrackerInfo,secrets:TrackerSecrets):{credentials:ProviderCredentials|null}|{problem:string}{
  switch(info.credential){
    case 'none':return {credentials:null};
    case 'api_key':return secrets.apiKey.trim()?{credentials:{kind:'api_key',api_key:secrets.apiKey.trim()}}:{problem:`${info.secretLabels[0]} is required.`};
    case 'account':return secrets.username.trim()&&secrets.password.trim()?{credentials:{kind:'username_password',username:secrets.username.trim(),password:secrets.password.trim()}}:{problem:`${info.secretLabels[0]} and ${info.secretLabels[1]} are required.`};
    case 'feed':{
      const tv=parseParameters(secrets.tvParameters,info.numericParameterNames),movies=parseParameters(secrets.movieParameters,info.numericParameterNames),cookie=secrets.cookie.trim();
      if(tv===null||movies===null)return {problem:'Feed parameters are name=value lines (at most 16, distinct names).'};
      if(cookie&&!/^[\u0020-\u007e]{1,4096}$/.test(cookie))return {problem:'The cookie must be printable ASCII.'};
      if(!cookie&&!tv.length&&!movies.length)return {credentials:null};
      return {credentials:{kind:'feed',...(cookie?{cookie}:{}),tv_parameters:tv,movie_parameters:movies}};
    }
  }
}
