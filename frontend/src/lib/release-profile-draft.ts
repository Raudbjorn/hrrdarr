import type {MediaDomain,MovieReleaseProfileInput,TvReleaseProfileInput,ReleaseProfile,ReleaseProfileIndexer,Provider,Tag} from './api.generated';
export type Input=MovieReleaseProfileInput|TvReleaseProfileInput;
export type Draft={domain:MediaDomain;value:Input;unnamed:boolean;name:string;grace:string};
export const eligible=(p:Provider,d:MediaDomain)=>p.settings.implementation!=='qbittorrent'&&p.settings[d]!==null;
export const referenceKey=(r:ReleaseProfileIndexer)=>r.kind==='provider'?`provider:${r.id}`:`source:${r.application}:${r.fingerprint}:${r.source_id}`;
export function fromInput(domain:MediaDomain,input:Input|ReleaseProfile):Draft {
 const v:Input={name:input.name,enabled:input.enabled,required:[...input.required],ignored:[...input.ignored],tag_ids:[...input.tag_ids],indexers:input.indexers.map(r=>({...r}))};
 if(domain==='tv'&&'excluded_tag_ids' in input)Object.assign(v,{excluded_tag_ids:[...input.excluded_tag_ids],air_date_restriction:input.air_date_restriction,air_date_grace_period_days:input.air_date_grace_period_days,allow_season_pack_without_all_episodes_aired:input.allow_season_pack_without_all_episodes_aired});
 return {domain,value:v,unnamed:input.name===null,name:input.name??'',grace:'air_date_grace_period_days' in input?String(input.air_date_grace_period_days):'0'};
}
export function toInput(d:Draft):Input {const v=fromInput(d.domain,d.value).value;v.name=d.unnamed?null:d.name;if('air_date_grace_period_days' in v)v.air_date_grace_period_days=Number(d.grace);return v;}
const bytes=(s:string)=>new TextEncoder().encode(s).length;
export function validate(d:Draft,tags:Tag[]):string {
 const v=d.value;
 if(!d.unnamed&&bytes(d.name)>256)return 'Name exceeds 256 UTF-8 bytes.';
 if(v.required.length>200||v.ignored.length>200)return 'Use at most 200 terms per list.';
 if([...v.required,...v.ignored].some(t=>!t.trim()||bytes(t)>2048))return 'Each term must contain text and use at most 2048 UTF-8 bytes.';
 if(!v.required.length&&!v.ignored.length&& !('air_date_restriction' in v&&(v.air_date_restriction||v.allow_season_pack_without_all_episodes_aired)))return 'Add a required or ignored term, or a TV air-date/pack rule.';
 const excluded='excluded_tag_ids' in v?v.excluded_tag_ids:[];
 if(v.tag_ids.length+excluded.length>200||[...v.tag_ids,...excluded].some(id=>!tags.some(t=>t.id===id))||new Set([...v.tag_ids,...excluded]).size!==v.tag_ids.length+excluded.length)return 'Choose up to 200 existing, disjoint include/exclude tags.';
 if('air_date_restriction' in v&&(!/^-?\d+$/.test(d.grace)||!Number.isSafeInteger(Number(d.grace))||Number(d.grace)<-2147483648||Number(d.grace)>2147483647))return 'Grace must be a whole number of days in the signed 32-bit range.';
 if(v.indexers.length>(d.domain==='tv'?200:1)||new Set(v.indexers.map(referenceKey)).size!==v.indexers.length)return 'Choose distinct indexers (up to 200 for TV, one for movies).';
 if(v.enabled&&v.indexers.some(r=>r.kind==='unresolved_source'))return 'Resolve or explicitly remove unavailable source indexers before enabling.';
 return '';
}
export function moveTerm(values:string[],index:number,delta:number):string[]{const out=[...values],next=index+delta;if(next>=0&&next<out.length)[out[index],out[next]]=[out[next],out[index]];return out;}
