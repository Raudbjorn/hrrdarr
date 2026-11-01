-- Retained causal evidence is independent of retry-control errors and survives checkpoints.
-- NULL on historical rows means unclassified; it is not proof of a successful import.
ALTER TABLE import_journal ADD COLUMN path_failure_code TEXT
CHECK(path_failure_code IS NULL OR
 (typeof(path_failure_code)='text' AND length(CAST(path_failure_code AS BLOB))<=64
  AND instr(path_failure_code,char(0))=0 AND path_failure_code IN (
   'unresolved_attempt',
   'source_missing_precommit',
   'source_missing_cleanup',
   'source_denied_precommit',
   'source_denied_cleanup',
   'source_changed_precommit',
   'source_changed_cleanup',
   'source_invalid_precommit',
   'source_invalid_cleanup',
   'source_io_precommit',
   'source_io_cleanup',
   'root_missing_precommit',
   'root_missing_cleanup',
   'root_denied_precommit',
   'root_denied_cleanup',
   'root_changed_precommit',
   'root_changed_cleanup',
   'root_invalid_precommit',
   'root_invalid_cleanup',
   'root_io_precommit',
   'root_io_cleanup',
   'destination_missing_precommit',
   'destination_missing_cleanup',
   'destination_denied_precommit',
   'destination_denied_cleanup',
   'destination_changed_precommit',
   'destination_changed_cleanup',
   'destination_invalid_precommit',
   'destination_invalid_cleanup',
   'destination_io_precommit',
   'destination_io_cleanup',
   'stage_missing_precommit',
   'stage_missing_cleanup',
   'stage_denied_precommit',
   'stage_denied_cleanup',
   'stage_changed_precommit',
   'stage_changed_cleanup',
   'stage_invalid_precommit',
   'stage_invalid_cleanup',
   'stage_io_precommit',
   'stage_io_cleanup',
   'retirement_missing_cleanup',
   'retirement_denied_cleanup',
   'retirement_changed_cleanup',
   'retirement_invalid_cleanup',
   'retirement_io_cleanup'
 )));

CREATE INDEX import_journal_path_unresolved ON import_journal(operation_id)
WHERE phase!='complete' AND
 (path_failure_code IS NOT NULL OR error_code IS NOT NULL OR phase!='preview');
CREATE INDEX download_processing_path_unresolved ON download_processing(candidate_id)
WHERE status NOT IN ('imported','cancelled');

-- Both checks start unobserved; the existing startup/scheduled lifecycle owns evaluation.
INSERT INTO health_checks(scope,check_key,startup,scheduled,compatibility_type) VALUES
 ('tv','remote_path_mapping',1,1,'RemotePathMappingCheck'),
 ('movies','remote_path_mapping',1,1,'RemotePathMappingCheck');
