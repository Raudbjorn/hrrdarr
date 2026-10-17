import type {MediaDomain,MovieIndexerScope,Provider,ProviderImplementation,ProviderInput,TvIndexerScope} from './api.generated';
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
const protocols:Record<ProviderImplementation,TransferProtocol>={torznab:'torrent',qbittorrent:'torrent',newznab:'usenet',sabnzbd:'usenet'};
export const protocolOf=(implementation:ProviderImplementation):TransferProtocol=>protocols[implementation];
export const isIndexerImplementation=(implementation:ProviderImplementation):implementation is 'torznab'|'newznab'=>implementation==='torznab'||implementation==='newznab';
export const isClientImplementation=(implementation:ProviderImplementation):implementation is 'qbittorrent'|'sabnzbd'=>implementation==='qbittorrent'||implementation==='sabnzbd';
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
