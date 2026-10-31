-- Import decisions are domain-scoped; these settings do not schedule RSS work.
CREATE TABLE import_policies (
 media_type TEXT NOT NULL PRIMARY KEY CHECK(media_type IN ('tv','movies')),
 skip_free_space_check_when_importing INTEGER NOT NULL DEFAULT 0
  CHECK(typeof(skip_free_space_check_when_importing)='integer' AND skip_free_space_check_when_importing IN (0,1)),
 minimum_free_space_mib INTEGER NOT NULL DEFAULT 100
  CHECK(typeof(minimum_free_space_mib)='integer' AND minimum_free_space_mib BETWEEN 100 AND 2147483647),
 download_client_working_folders TEXT NOT NULL DEFAULT '_UNPACK_|_FAILED_'
  CHECK(typeof(download_client_working_folders)='text' AND length(CAST(download_client_working_folders AS BLOB)) BETWEEN 1 AND 4096),
 revision INTEGER NOT NULL DEFAULT 1
  CHECK(typeof(revision)='integer' AND revision BETWEEN 1 AND 9007199254740991),
 locally_edited INTEGER NOT NULL DEFAULT 0
  CHECK(typeof(locally_edited)='integer' AND locally_edited IN (0,1))
);
INSERT INTO import_policies(media_type) VALUES('tv'),('movies');
-- Historical retained Config may activate once, without overriding native edits.
ALTER TABLE snapshot_imports ADD COLUMN import_policy_version INTEGER NOT NULL DEFAULT 0
 CHECK(typeof(import_policy_version)='integer' AND import_policy_version IN (0,1));
