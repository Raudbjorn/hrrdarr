import type {Provider,ProviderInput} from './api.generated';
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
