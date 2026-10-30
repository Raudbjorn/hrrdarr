-- hc.034 RootFolderCheck (TV, movies) and MovieCollectionRootFolderCheck (movies), and hc.035
-- MountCheck (TV, movies). Observation-only identities: detectors never create, write into or
-- probe-write library folders. Identities start unobserved; startup/scheduled/manual own runs.
INSERT INTO health_checks(scope,check_key,startup,scheduled,compatibility_type) VALUES
 ('tv','root_folder',1,1,'RootFolderCheck'),
 ('movies','root_folder',1,1,'RootFolderCheck'),
 ('movies','collection_root_folder',1,1,'MovieCollectionRootFolderCheck'),
 ('tv','mount_read_only',1,1,'MountCheck'),
 ('movies','mount_read_only',1,1,'MountCheck');
