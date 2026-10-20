import type {ImportPolicyUpdate} from './api.generated';
import type {Result} from './api';

export const MIN_FREE_SPACE_MIB = 100;
export const MAX_FREE_SPACE_MIB = 2147483647;
export const MAX_WORKING_FOLDER_BYTES = 4096;
export type ImportPolicyDraft = Omit<ImportPolicyUpdate,'revision'|'minimum_free_space_mib'> & {minimum_free_space_mib:number|undefined};

export function importPolicyUpdate(revision:number,draft:ImportPolicyDraft):Result<ImportPolicyUpdate> {
  const minimum=draft.minimum_free_space_mib;
  if(minimum===undefined||!Number.isSafeInteger(minimum)||minimum<MIN_FREE_SPACE_MIB||minimum>MAX_FREE_SPACE_MIB) return {ok:false,error:`Minimum free space must be a whole number from ${MIN_FREE_SPACE_MIB} to ${MAX_FREE_SPACE_MIB} MiB.`};
  if(new TextEncoder().encode(draft.download_client_working_folders).length>MAX_WORKING_FOLDER_BYTES) return {ok:false,error:`Download-client working folder prefixes must fit within ${MAX_WORKING_FOLDER_BYTES} UTF-8 bytes.`};
  return {ok:true,data:{revision,skip_free_space_check_when_importing:draft.skip_free_space_check_when_importing,minimum_free_space_mib:minimum,download_client_working_folders:draft.download_client_working_folders}};
}
