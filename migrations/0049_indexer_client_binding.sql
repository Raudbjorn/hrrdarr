-- Preserve a broken preference for health reporting; deliberately no target FK or SET NULL.
-- The existing owner FK, provider revision rules and all scoped option guards remain intact.
ALTER TABLE provider_scopes ADD COLUMN download_client_id TEXT DEFAULT NULL CHECK (
    download_client_id IS NULL OR (
        implementation IN ('torznab','newznab')
        AND typeof(download_client_id)='text'
        AND length(download_client_id)=36
        AND length(CAST(download_client_id AS BLOB))=36
        AND instr(download_client_id,char(0))=0
        AND substr(download_client_id,9,1)='-'
        AND substr(download_client_id,14,1)='-'
        AND substr(download_client_id,19,1)='-'
        AND substr(download_client_id,24,1)='-'
        AND length(replace(download_client_id,'-',''))=32
        AND replace(download_client_id,'-','') NOT GLOB '*[^0-9a-f]*'
    )
);
INSERT INTO health_checks(scope,check_key,startup,scheduled,compatibility_type)VALUES
 ('tv','indexer_download_client',1,1,'IndexerDownloadClientCheck'),
 ('movies','indexer_download_client',1,1,'IndexerDownloadClientCheck');
