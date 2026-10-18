import type { DelayProfile, DelayProfileInput, DelayProfileSettings, Tag } from './api.generated';
export type DelayDraft = Omit<DelayProfileSettings,'torrent_delay_minutes'|'usenet_delay_minutes'|'minimum_custom_format_score'> & {torrent:string;usenet:string;score:string;tag_ids:number[]};
export function draftFrom(settings:DelayProfileInput, existing?:DelayProfile):DelayDraft {
 const full=existing?.settings.semantics==='profile'?existing.settings:settings.settings;
 return {enable_torrent:full.enable_torrent,enable_usenet:full.enable_usenet,preferred_protocol:full.preferred_protocol,bypass_if_highest_quality:full.bypass_if_highest_quality,bypass_if_above_custom_format_score:full.bypass_if_above_custom_format_score,torrent:String(existing?.settings.torrent_delay_minutes??full.torrent_delay_minutes),usenet:String(existing?.settings.usenet_delay_minutes??full.usenet_delay_minutes),score:String(full.minimum_custom_format_score),tag_ids:[...(existing?.tag_ids??settings.tag_ids)]};
}
export function integer(value:string,min:number,max:number):number|null {
 if(!/^-?\d+$/.test(value))return null;const number=Number(value);return Number.isSafeInteger(number)&&number>=min&&number<=max?number:null;
}
export function draftError(draft:DelayDraft,global:boolean,tags:Tag[],profiles:DelayProfile[],id:number|null):string {
 if(integer(draft.torrent,0,10080)===null||integer(draft.usenet,0,10080)===null)return 'Delays must be whole minutes from 0 to 10080.';
 if(integer(draft.score,-2147483648,2147483647)===null)return 'Minimum custom format score must be a signed 32-bit integer.';
 if(!draft.enable_torrent&&!draft.enable_usenet)return 'Enable at least one download protocol.';
 if(!['torrent','usenet'].includes(draft.preferred_protocol))return 'Choose a preferred protocol.';
 if(global&&draft.tag_ids.length)return 'The global fallback cannot have tags.';
 if(!global&&!draft.tag_ids.length)return 'Choose at least one tag for a tagged profile.';
 if(draft.tag_ids.length>200||new Set(draft.tag_ids).size!==draft.tag_ids.length||draft.tag_ids.some(id=>!tags.some(tag=>tag.id===id)))return 'Choose up to 200 distinct current tags; reload stale references.';
 if(draft.tag_ids.some(tag=>profiles.some(profile=>profile.id!==id&&profile.tag_ids.includes(tag))))return 'A selected tag already belongs to another delay profile.';
 return '';
}
export function draftInput(draft:DelayDraft):DelayProfileInput {
 return {settings:{torrent_delay_minutes:Number(draft.torrent),usenet_delay_minutes:Number(draft.usenet),enable_torrent:draft.enable_torrent,enable_usenet:draft.enable_usenet,preferred_protocol:draft.preferred_protocol,bypass_if_highest_quality:draft.bypass_if_highest_quality,bypass_if_above_custom_format_score:draft.bypass_if_above_custom_format_score,minimum_custom_format_score:Number(draft.score)},tag_ids:[...draft.tag_ids]};
}
export function moveProfile(ids:number[],id:number,step:number):number[]{const at=ids.indexOf(id),next=at+step;if(at<0||next<0||next>=ids.length)return ids;const copy=[...ids];[copy[at],copy[next]]=[copy[next],copy[at]];return copy;}
