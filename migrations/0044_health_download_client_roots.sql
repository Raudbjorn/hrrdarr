-- New diagnostics start unobserved, never as a successful check or import permission.
-- The existing startup lifecycle marks both keys pending and advances their generations.
-- Preserve all existing pending reasons, observations, command membership and event cursors.
INSERT INTO health_checks(scope,check_key,startup,scheduled,compatibility_type)VALUES
 ('tv','download_client_root_folder',1,1,'DownloadClientRootFolderCheck'),
 ('movies','download_client_root_folder',1,1,'DownloadClientRootFolderCheck');
