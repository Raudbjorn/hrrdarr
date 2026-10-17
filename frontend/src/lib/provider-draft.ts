import type {MediaDomain,MovieIndexerScope,Provider,ProviderInput,TvIndexerScope} from './api.generated';
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

export function indexerScope(provider:Provider|null,domain:MediaDomain) {
  return provider && provider.settings.implementation!=='qbittorrent' ? provider.settings[domain] : null;
}
export function indexerOptions(scope:TvIndexerScope|MovieIndexerScope|null|undefined) {
  return {enable_rss:scope?.enable_rss??true,enable_automatic_search:scope?.enable_automatic_search??true,enable_interactive_search:scope?.enable_interactive_search??true,download_client_id:scope?.download_client_id??null};
}
export function eligibleDownloadClient(provider:Provider|null,domain:MediaDomain):provider is Provider {
  return !!provider && provider.enabled && provider.settings.implementation==='qbittorrent' && provider.settings[domain]!==null;
}
const clientAttemptLabels:Record<string,string>={selected:'Client selected',prepared:'Ready to send',dispatching:'Sending',definitely_not_submitted:'Not submitted',observed:'Confirmed in client',unknown:'Outcome uncertain'};
export const clientAttemptLabel=(state:string)=>clientAttemptLabels[state]??state;
