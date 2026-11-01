-- Complete normalized observations belong to immutable children, not transfer plans.
-- SHA256 is verified over normalized UTF-8 bytes by the application, not SQL.
CREATE TABLE downloaded_scan_media_observations (
 child_id TEXT NOT NULL PRIMARY KEY REFERENCES downloaded_scan_children(id) ON DELETE RESTRICT,
 command_id TEXT NOT NULL REFERENCES downloaded_scan_commands(id) ON DELETE RESTRICT,
 media_info_json TEXT NOT NULL CHECK(typeof(media_info_json)='text' AND length(CAST(media_info_json AS BLOB)) BETWEEN 1 AND 65536 AND instr(media_info_json,char(0))=0) CHECK(json_valid(media_info_json) AND json_type(media_info_json)='object'),
 sha256 TEXT NOT NULL CHECK(typeof(sha256)='text' AND length(CAST(sha256 AS BLOB))=64 AND instr(sha256,char(0))=0 AND sha256 NOT GLOB '*[^0-9a-f]*'),
 observation_bytes INTEGER GENERATED ALWAYS AS (length(CAST(media_info_json AS BLOB))+64+128) STORED
);
CREATE INDEX downloaded_scan_media_bytes ON downloaded_scan_media_observations(command_id,observation_bytes);
CREATE TRIGGER scan_media_immutable BEFORE UPDATE ON downloaded_scan_media_observations
BEGIN SELECT RAISE(ABORT,'scan media observation is immutable'); END;
CREATE TRIGGER scan_media_retained BEFORE DELETE ON downloaded_scan_media_observations
BEGIN SELECT RAISE(ABORT,'scan media observation must be retained'); END;

-- Optional descriptors are validated too; absent and explicit JSON null are distinct.
CREATE VIEW downloaded_scan_invalid_observations AS
SELECT c.id,c.command_id FROM downloaded_scan_children c
WHERE (c.selected=1 OR json_type(c.decision_json,'$.observation') IS NOT NULL)
 AND NOT coalesce((json_type(c.decision_json,'$.observation') IS 'object'
 AND (SELECT count(*) FROM json_each(c.decision_json,'$.observation'))=(SELECT count(DISTINCT key) FROM json_each(c.decision_json,'$.observation'))
 AND json_type(c.decision_json,'$.observation.version') IS 'integer'
 AND json_extract(c.decision_json,'$.observation.version') IS 1
 AND ((json_extract(c.decision_json,'$.observation.state') IS 'measured'
       AND json_type(c.decision_json,'$.observation.sha256') IS 'text'
       AND length(CAST(json_extract(c.decision_json,'$.observation.sha256') AS BLOB))=64
       AND instr(json_extract(c.decision_json,'$.observation.sha256'),char(0))=0
       AND json_extract(c.decision_json,'$.observation.sha256') NOT GLOB '*[^0-9a-f]*'
       AND json_type(c.decision_json,'$.observation.reason') IS NULL)
   OR (json_extract(c.decision_json,'$.observation.state') IS 'unavailable'
       AND json_type(c.decision_json,'$.observation.sha256') IS NULL
       AND json_type(c.decision_json,'$.observation.reason') IS 'text'
       AND coalesce(json_extract(c.decision_json,'$.observation.reason') IN ('probe_cancelled','probe_descriptor_failed','probe_duration_invalid','probe_executable_missing','probe_failed','probe_frame_rate_invalid','probe_integer_invalid','probe_json_invalid','probe_kill_failed','probe_media_invalid','probe_metadata_limit','probe_normalization_invalid','probe_normalization_limit','probe_object_invalid','probe_output_failed','probe_output_limit','probe_output_missing','probe_reap_failed','probe_reported_error','probe_spawn_failed','probe_stream_limit','probe_stream_type_invalid','probe_stream_type_missing','probe_streams_invalid','probe_streams_missing','probe_text_invalid','probe_timeout','probe_wait_failed'),0)))),0);
CREATE TRIGGER scan_child_observation_shape AFTER INSERT ON downloaded_scan_children
WHEN EXISTS(SELECT 1 FROM downloaded_scan_invalid_observations v WHERE v.id=NEW.id)
BEGIN SELECT RAISE(ABORT,'invalid scan observation descriptor'); END;

CREATE TRIGGER scan_media_admit BEFORE INSERT ON downloaded_scan_media_observations
BEGIN
 SELECT CASE WHEN EXISTS(SELECT 1 FROM downloaded_scan_media_observations WHERE child_id=NEW.child_id) THEN RAISE(ABORT,'scan media observation is immutable') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM downloaded_scan_children c JOIN downloaded_scan_commands p ON p.id=c.command_id WHERE c.id=NEW.child_id AND c.command_id=NEW.command_id AND p.status='running' AND p.inventory_revision=0 AND p.cancel_requested=0 AND json_extract(c.decision_json,'$.observation.state') IS 'measured' AND json_extract(c.decision_json,'$.observation.sha256') IS NEW.sha256 AND NOT EXISTS(SELECT 1 FROM downloaded_scan_invalid_observations v WHERE v.id=c.id)) THEN RAISE(ABORT,'scan media requires matching unpublished child') END;
 SELECT CASE WHEN (SELECT bytes FROM downloaded_scan_inventory_size WHERE id=NEW.command_id)+length(CAST(NEW.media_info_json AS BLOB))+64+128>67108864 THEN RAISE(ABORT,'scan inventory byte capacity reached') END;
END;

-- Only new publication is gated. Previously published56 facts are retained untouched.
CREATE TRIGGER scan_observations_publish BEFORE UPDATE OF inventory_revision ON downloaded_scan_commands
WHEN OLD.inventory_revision=0 AND NEW.inventory_revision=1
BEGIN
 SELECT CASE WHEN EXISTS(SELECT 1 FROM downloaded_scan_invalid_observations v WHERE v.command_id=NEW.id) OR EXISTS(SELECT 1 FROM downloaded_scan_children c LEFT JOIN downloaded_scan_media_observations m ON m.child_id=c.id WHERE c.command_id=NEW.id AND ((json_extract(c.decision_json,'$.observation.state') IS 'measured' AND (m.child_id IS NULL OR m.command_id IS NOT c.command_id OR m.sha256 IS NOT json_extract(c.decision_json,'$.observation.sha256'))) OR (json_extract(c.decision_json,'$.observation.state') IS NOT 'measured' AND m.child_id IS NOT NULL))) THEN RAISE(ABORT,'scan observation inventory incomplete') END;
END;

DROP VIEW downloaded_scan_inventory_size;
CREATE VIEW downloaded_scan_inventory_size AS
SELECT p.id,
 (SELECT count(*) FROM downloaded_scan_children c WHERE c.command_id=p.id) AS child_count,
 (SELECT count(*) FROM downloaded_scan_child_episodes m WHERE m.command_id=p.id) AS member_count,
 (SELECT count(*) FROM downloaded_scan_coverage v WHERE v.command_id=p.id) AS expected_count,
 coalesce((SELECT sum(c.manifest_bytes) FROM downloaded_scan_children c WHERE c.command_id=p.id),0)
 +64*(SELECT count(*) FROM downloaded_scan_child_episodes m WHERE m.command_id=p.id)
 +64*(SELECT count(*) FROM downloaded_scan_coverage v WHERE v.command_id=p.id)
 +coalesce((SELECT sum(m.observation_bytes) FROM downloaded_scan_media_observations m WHERE m.command_id=p.id),0)
 +coalesce(length(CAST(p.inventory_json AS BLOB)),0) AS bytes
FROM downloaded_scan_commands p;

-- Preserve all existing transition authority; only account the pending envelope delta.
DROP TRIGGER downloaded_scan_transition;
CREATE TRIGGER downloaded_scan_transition BEFORE UPDATE ON downloaded_scan_commands BEGIN
 SELECT CASE WHEN NEW.id IS NOT OLD.id OR NEW.media_type IS NOT OLD.media_type OR NEW.source_path IS NOT OLD.source_path OR NEW.requested_mode IS NOT OLD.requested_mode OR NEW.requested_provider_id IS NOT OLD.requested_provider_id OR NEW.requested_remote_id IS NOT OLD.requested_remote_id OR NEW.supersedes_command_id IS NOT OLD.supersedes_command_id OR NEW.priority IS NOT OLD.priority OR NEW.created_at IS NOT OLD.created_at THEN RAISE(ABORT,'scan request is immutable') END;
 SELECT CASE WHEN OLD.status IN ('succeeded','failed','cancelled') THEN RAISE(ABORT,'terminal scan proof is immutable') END;
 SELECT CASE WHEN NEW.cancel_requested<OLD.cancel_requested OR NEW.inventory_revision<OLD.inventory_revision OR NEW.run_generation<OLD.run_generation OR NEW.run_generation>OLD.run_generation+1 OR NEW.ready_order_at<OLD.ready_order_at THEN RAISE(ABORT,'scan generations cannot regress or skip') END;
 SELECT CASE WHEN NOT ((OLD.status=NEW.status) OR (OLD.status='queued' AND NEW.status IN ('running','cancelled')) OR (OLD.status='retry_wait' AND NEW.status IN ('running','cancelled','failed')) OR (OLD.status='running' AND NEW.status IN ('retry_wait','succeeded','failed','cancelled'))) THEN RAISE(ABORT,'invalid scan transition') END;
 SELECT CASE WHEN NEW.attempts!=OLD.attempts+CASE WHEN OLD.status IN ('queued','retry_wait') AND NEW.status='running' THEN 1 ELSE 0 END THEN RAISE(ABORT,'scan attempts count claims not children') END;
 SELECT CASE WHEN (OLD.status IN ('queued','retry_wait') AND NEW.status='running' AND NEW.run_generation!=OLD.run_generation+1) OR (NEW.run_generation!=OLD.run_generation AND NEW.status!='running') THEN RAISE(ABORT,'scan claim requires generation') END;
 SELECT CASE WHEN NEW.status='retry_wait' AND (OLD.inventory_revision!=0 OR EXISTS(SELECT 1 FROM downloaded_scan_children WHERE command_id=OLD.id AND operation_id IS NOT NULL)) THEN RAISE(ABORT,'effectful scan cannot automatically retry') END;
 SELECT CASE WHEN OLD.inventory_revision=1 AND (NEW.inventory_revision IS NOT OLD.inventory_revision OR NEW.inventory_json IS NOT OLD.inventory_json OR NEW.inventory_count IS NOT OLD.inventory_count OR NEW.inventory_bytes IS NOT OLD.inventory_bytes OR NEW.candidate_id IS NOT OLD.candidate_id OR NEW.predecessor_operation_id IS NOT OLD.predecessor_operation_id OR NEW.tracked_json IS NOT OLD.tracked_json OR NEW.processing_fence_json IS NOT OLD.processing_fence_json) THEN RAISE(ABORT,'published scan inventory is immutable') END;
 SELECT CASE WHEN OLD.candidate_id IS NOT NULL AND (NEW.candidate_id IS NOT OLD.candidate_id OR NEW.predecessor_operation_id IS NOT OLD.predecessor_operation_id OR NEW.tracked_json IS NOT OLD.tracked_json OR NEW.processing_fence_json IS NOT OLD.processing_fence_json) THEN RAISE(ABORT,'captured scan receipt is immutable') END;
 SELECT CASE WHEN NEW.candidate_id IS NOT OLD.candidate_id AND (NEW.status!='running' OR OLD.inventory_revision!=0 OR NOT EXISTS(SELECT 1 FROM rss_candidates r JOIN providers v ON v.id=r.client_id JOIN provider_scopes s ON s.provider_id=v.id AND s.media_type=r.media_type WHERE r.id=NEW.candidate_id AND r.status='observed' AND r.media_type=NEW.media_type AND (NEW.requested_provider_id IS NULL OR (NEW.requested_provider_id=r.client_id AND NEW.requested_remote_id=r.observed_hash)) AND v.enabled=1 AND v.revision=r.client_revision AND (json_type(NEW.tracked_json,'$.version') IS 'integer' AND json_extract(NEW.tracked_json,'$.version') IS 1) AND json_extract(NEW.tracked_json,'$.provider_id') IS r.client_id AND json_extract(NEW.tracked_json,'$.provider_revision') IS r.client_revision AND json_extract(NEW.tracked_json,'$.remote_id') IS r.observed_hash) OR EXISTS(SELECT 1 FROM download_processing d WHERE d.candidate_id=NEW.candidate_id AND d.status='checking') OR EXISTS(SELECT 1 FROM rss_candidate_imports i WHERE i.candidate_id=NEW.candidate_id AND i.operation_id IS NOT NEW.predecessor_operation_id) OR (NEW.predecessor_operation_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM rss_candidate_imports i WHERE i.candidate_id=NEW.candidate_id AND i.operation_id=NEW.predecessor_operation_id))) THEN RAISE(ABORT,'invalid or busy scan receipt authority') END;
 SELECT CASE WHEN NEW.candidate_id IS NOT OLD.candidate_id AND EXISTS(SELECT 1 FROM downloaded_scan_commands p WHERE p.candidate_id=NEW.candidate_id AND p.id!=NEW.id AND (p.status IN ('queued','running','retry_wait') OR (p.id NOT IN (WITH RECURSIVE ancestry(id,previous,depth) AS (SELECT id,supersedes_command_id,1 FROM downloaded_scan_commands WHERE id=NEW.supersedes_command_id UNION ALL SELECT a.id,a.supersedes_command_id,x.depth+1 FROM downloaded_scan_commands a JOIN ancestry x ON a.id=x.previous WHERE x.depth<1024) SELECT id FROM ancestry) AND p.coverage_state IN ('complete','incomplete')))) THEN RAISE(ABORT,'tracked receipt requires explicit scan lineage') END;
 SELECT CASE WHEN NEW.supersedes_command_id IS NOT NULL AND NEW.candidate_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM downloaded_scan_commands p WHERE p.id=NEW.supersedes_command_id AND p.candidate_id=NEW.candidate_id AND p.coverage_state='incomplete') THEN RAISE(ABORT,'scan successor candidate changed') END;
 SELECT CASE WHEN NEW.inventory_revision!=OLD.inventory_revision AND (NEW.status!='running' OR NEW.cancel_requested!=0 OR (json_type(NEW.inventory_json,'$.version') IS NOT 'integer' OR json_extract(NEW.inventory_json,'$.version') IS NOT 1) OR NOT EXISTS(SELECT 1 FROM downloaded_scan_inventory_size s WHERE s.id=NEW.id AND NEW.inventory_count=s.child_count AND NEW.inventory_bytes=s.bytes+coalesce(length(CAST(NEW.inventory_json AS BLOB)),0)-coalesce(length(CAST(OLD.inventory_json AS BLOB)),0) AND s.member_count<=20000 AND s.expected_count<=20000 AND s.bytes+coalesce(length(CAST(NEW.inventory_json AS BLOB)),0)-coalesce(length(CAST(OLD.inventory_json AS BLOB)),0)<=67108864)) THEN RAISE(ABORT,'scan inventory must publish completely') END;
 SELECT CASE WHEN NEW.inventory_revision!=OLD.inventory_revision AND (EXISTS(SELECT 1 FROM downloaded_scan_children c WHERE c.command_id=NEW.id AND c.selected=1 AND c.series_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM downloaded_scan_child_episodes m WHERE m.child_id=c.id)) OR (SELECT count(DISTINCT coalesce(series_id,movie_id)) FROM downloaded_scan_children WHERE command_id=NEW.id AND selected=1)>1) THEN RAISE(ABORT,'scan inventory requires complete single owner membership') END;
 SELECT CASE WHEN NEW.inventory_revision!=OLD.inventory_revision AND NEW.candidate_id IS NOT NULL AND (NOT EXISTS(SELECT 1 FROM downloaded_scan_coverage v WHERE v.command_id=NEW.id) OR EXISTS(SELECT 1 FROM rss_candidate_episodes e WHERE e.candidate_id=NEW.candidate_id AND NOT EXISTS(SELECT 1 FROM downloaded_scan_coverage v WHERE v.command_id=NEW.id AND v.episode_id=e.episode_id))) THEN RAISE(ABORT,'scan expected coverage must be complete') END;
 SELECT CASE WHEN NEW.cleanup_resume_generation NOT IN (OLD.cleanup_resume_generation,OLD.cleanup_resume_generation+1) OR (NEW.cleanup_resume_generation=OLD.cleanup_resume_generation+1 AND (NEW.cleanup_resume_requested!=1 OR OLD.cleanup_resume_requested!=0 OR OLD.cleanup_state!='pending')) OR (NEW.cleanup_resume_requested>OLD.cleanup_resume_requested AND NEW.cleanup_resume_generation!=OLD.cleanup_resume_generation+1) THEN RAISE(ABORT,'invalid cleanup resume generation') END;
 SELECT CASE WHEN OLD.cleanup_state IN ('complete','retained') AND (NEW.cleanup_state IS NOT OLD.cleanup_state OR NEW.cleanup_json IS NOT OLD.cleanup_json OR NEW.cleanup_resume_generation IS NOT OLD.cleanup_resume_generation OR NEW.cleanup_resume_requested IS NOT OLD.cleanup_resume_requested) THEN RAISE(ABORT,'settled cleanup is immutable') END;
 SELECT CASE WHEN OLD.cleanup_json IS NOT NULL AND (json_extract(NEW.cleanup_json,'$.root_identity.dev','$.root_identity.ino','$.parent_identity.dev','$.parent_identity.ino') IS NOT json_extract(OLD.cleanup_json,'$.root_identity.dev','$.root_identity.ino','$.parent_identity.dev','$.parent_identity.ino') OR (json_extract(OLD.cleanup_json,'$.phase')='remove_intent' AND json_extract(NEW.cleanup_json,'$.phase') NOT IN ('remove_intent','settled'))) THEN RAISE(ABORT,'cleanup authority cannot regress') END;
 SELECT CASE WHEN NEW.cleanup_state!='not_needed' AND NOT EXISTS(SELECT 1 FROM downloaded_scan_children c WHERE c.command_id=NEW.id AND c.disposition='imported' AND c.resolved_mode='move') THEN RAISE(ABORT,'cleanup requires imported move') END;
 SELECT CASE WHEN NEW.status IN ('succeeded','failed','cancelled') AND (NEW.cleanup_state='pending' OR NEW.cleanup_resume_requested!=0 OR EXISTS(SELECT 1 FROM downloaded_scan_children c LEFT JOIN import_journal j ON j.operation_id=c.operation_id WHERE c.command_id=NEW.id AND (c.disposition IN ('ready','importing') OR (c.operation_id IS NOT NULL AND j.phase!='complete' AND NOT(c.disposition='cancelled' AND j.phase='preview' AND j.error_code IS NULL AND j.path_failure_code IS NULL))))) THEN RAISE(ABORT,'scan effects must settle before terminal state') END;
 SELECT CASE WHEN NEW.status='succeeded' AND (NEW.inventory_revision!=1 OR (NEW.outcome='success')!=EXISTS(SELECT 1 FROM downloaded_scan_children c WHERE c.command_id=NEW.id AND c.disposition='imported')) THEN RAISE(ABORT,'scan outcome must match new imports') END;
 SELECT CASE WHEN NEW.status IN ('succeeded','failed','cancelled') AND NEW.candidate_id IS NOT NULL AND (NEW.coverage_state NOT IN ('complete','incomplete') OR (NEW.coverage_state='complete')!=(EXISTS(SELECT 1 FROM downloaded_scan_coverage v WHERE v.command_id=NEW.id) AND NOT EXISTS(SELECT 1 FROM downloaded_scan_coverage v WHERE v.command_id=NEW.id AND v.credited_operation_id IS NULL))) THEN RAISE(ABORT,'scan coverage must match exact credited set') END;
 SELECT CASE WHEN NEW.coverage_state IN ('complete','incomplete') AND NEW.status NOT IN ('succeeded','failed','cancelled') THEN RAISE(ABORT,'scan coverage settles with parent') END;
END;

-- The current-byte view includes the stored envelope once. During UPDATE use only its delta.
CREATE TRIGGER scan_envelope_byte_capacity BEFORE UPDATE OF inventory_json ON downloaded_scan_commands
WHEN (SELECT v.bytes+coalesce((SELECT sum(length(CAST(r.old_file_json AS BLOB))+4096) FROM scan_import_replacements r JOIN scan_import_facts f ON f.operation_id=r.operation_id WHERE f.command_id=v.id),0) FROM downloaded_scan_inventory_size v WHERE v.id=NEW.id)+coalesce(length(CAST(NEW.inventory_json AS BLOB)),0)-coalesce(length(CAST(OLD.inventory_json AS BLOB)),0)>67108864
BEGIN SELECT RAISE(ABORT,'scan inventory byte capacity reached'); END;
CREATE TRIGGER scan_member_byte_capacity BEFORE INSERT ON downloaded_scan_child_episodes
WHEN (SELECT bytes FROM downloaded_scan_inventory_size WHERE id=NEW.command_id)+64>67108864
BEGIN SELECT RAISE(ABORT,'scan inventory byte capacity reached'); END;
CREATE TRIGGER scan_coverage_byte_capacity BEFORE INSERT ON downloaded_scan_coverage
WHEN (SELECT bytes FROM downloaded_scan_inventory_size WHERE id=NEW.command_id)+64>67108864
BEGIN SELECT RAISE(ABORT,'scan inventory byte capacity reached'); END;
