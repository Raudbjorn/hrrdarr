import type {MediaDomain,MovieIndexerScope,Provider,ProviderImplementation,ProviderInput,TorrentPlacement,TvIndexerScope} from './api.generated';
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
const protocols:Record<ProviderImplementation,TransferProtocol>={torznab:'torrent',torrentrss:'torrent',qbittorrent:'torrent',transmission:'torrent',deluge:'torrent',rtorrent:'torrent',utorrent:'torrent',newznab:'usenet',sabnzbd:'usenet',nzbget:'usenet'};
export const protocolOf=(implementation:ProviderImplementation):TransferProtocol=>protocols[implementation];
export const isIndexerImplementation=(implementation:ProviderImplementation):implementation is 'torznab'|'newznab'=>implementation==='torznab'||implementation==='newznab';
export const isClientImplementation=(implementation:ProviderImplementation):implementation is 'qbittorrent'|'sabnzbd'|'nzbget'|'transmission'|'deluge'|'rtorrent'|'utorrent'=>implementation==='qbittorrent'||implementation==='sabnzbd'||implementation==='nzbget'||implementation==='transmission'||implementation==='deluge'||implementation==='rtorrent'||implementation==='utorrent';
export const isIndexer=(provider:Provider)=>isIndexerImplementation(provider.settings.implementation);
export const isDownloadClient=(provider:Provider)=>isClientImplementation(provider.settings.implementation);
export function indexerScope(provider:Provider|null,domain:MediaDomain) {
  return provider && (provider.settings.implementation==='torznab'||provider.settings.implementation==='newznab') ? provider.settings[domain] : null;
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
  const base=normalizeUrlBase(decodeURI(url.pathname));
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
