-- Downloaded-scan successor lineage reservation and bounded Health evidence indexes.
-- Earlier migrations are unchanged; this adds one admission trigger and two indexes.

-- A predecessor's lineage is held by at most one successor: one that is still active, or one
-- that captured the receipt (whatever its outcome). A successor that settled before capturing
-- never consumed the lineage, so another explicit successor may be admitted afterwards.
CREATE TRIGGER scan_successor_unique BEFORE INSERT ON downloaded_scan_commands
WHEN NEW.supersedes_command_id IS NOT NULL AND EXISTS(SELECT 1 FROM downloaded_scan_commands s WHERE s.supersedes_command_id=NEW.supersedes_command_id AND (s.status IN ('queued','running','retry_wait') OR s.candidate_id IS NOT NULL))
BEGIN SELECT RAISE(ABORT,'competing scan successor'); END;

-- Measured selected-child prepare refusals: failed before any operation existed.
CREATE INDEX downloaded_scan_prepare_refused ON downloaded_scan_children(command_id)
WHERE disposition='failed' AND operation_id IS NULL;

-- A later published request for the same domain and source supersedes earlier evidence.
CREATE INDEX downloaded_scan_source_request ON downloaded_scan_commands(media_type,source_path,created_at,id);
