-- Durable downloaded-folder producer. Existing operation journals remain the transfer authority.
-- The migration runner disables FK enforcement around its checked transaction; legacy rows
-- and all unrelated admission SQL are retained verbatim. Never edit earlier migrations.

CREATE TABLE downloaded_scan_commands (
 id TEXT NOT NULL CHECK(typeof(id)='text' AND length(id)=36 AND substr(id,9,1)='-' AND substr(id,14,1)='-' AND substr(id,19,1)='-' AND substr(id,24,1)='-' AND length(replace(id,'-',''))=32 AND replace(id,'-','') NOT GLOB '*[^0-9a-f]*') PRIMARY KEY,
 media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
 source_path TEXT NOT NULL CHECK(typeof(source_path)='text' AND length(CAST(source_path AS BLOB)) BETWEEN 1 AND 4096 AND instr(source_path,char(0))=0),
 requested_mode TEXT NOT NULL CHECK(requested_mode IN ('auto','move','copy')),
 requested_provider_id TEXT CHECK(requested_provider_id IS NULL OR (typeof(requested_provider_id)='text' AND length(CAST(requested_provider_id AS BLOB)) BETWEEN 1 AND 36 AND instr(requested_provider_id,char(0))=0)),
 requested_remote_id TEXT CHECK(requested_remote_id IS NULL OR (typeof(requested_remote_id)='text' AND length(CAST(requested_remote_id AS BLOB)) BETWEEN 1 AND 1024 AND instr(requested_remote_id,char(0))=0)),
 candidate_id TEXT REFERENCES rss_candidates(id) ON DELETE RESTRICT,
 predecessor_operation_id TEXT REFERENCES import_journal(operation_id) ON DELETE RESTRICT,
 supersedes_command_id TEXT REFERENCES downloaded_scan_commands(id) ON DELETE RESTRICT,
 tracked_json TEXT CHECK(tracked_json IS NULL OR (typeof(tracked_json)='text' AND length(CAST(tracked_json AS BLOB)) BETWEEN 1 AND 16384 AND instr(tracked_json,char(0))=0)) CHECK(tracked_json IS NULL OR (json_valid(tracked_json) AND json_type(tracked_json)='object')),
 processing_fence_json TEXT CHECK(processing_fence_json IS NULL OR (typeof(processing_fence_json)='text' AND length(CAST(processing_fence_json AS BLOB)) BETWEEN 1 AND 8192 AND instr(processing_fence_json,char(0))=0)) CHECK(processing_fence_json IS NULL OR (json_valid(processing_fence_json) AND json_type(processing_fence_json)='object')),
 status TEXT NOT NULL DEFAULT 'queued' CHECK(status IN ('queued','running','retry_wait','succeeded','failed','cancelled')),
 priority INTEGER NOT NULL DEFAULT 0 CHECK(typeof(priority)='integer' AND priority BETWEEN 0 AND 1),
 attempts INTEGER NOT NULL DEFAULT 0 CHECK(typeof(attempts)='integer' AND attempts BETWEEN 0 AND 3),
 run_generation INTEGER NOT NULL DEFAULT 0 CHECK(typeof(run_generation)='integer' AND run_generation BETWEEN 0 AND 9007199254740991),
 ready_order_at INTEGER NOT NULL CHECK(typeof(ready_order_at)='integer' AND ready_order_at BETWEEN 0 AND 9007199254740991),
 next_attempt_at INTEGER NOT NULL CHECK(typeof(next_attempt_at)='integer' AND next_attempt_at BETWEEN 0 AND 9007199254740991),
 created_at INTEGER NOT NULL CHECK(typeof(created_at)='integer' AND created_at BETWEEN 0 AND 9007199254740991),
 started_at INTEGER CHECK(started_at IS NULL OR (typeof(started_at)='integer' AND started_at BETWEEN 0 AND 9007199254740991)),
 completed_at INTEGER CHECK(completed_at IS NULL OR (typeof(completed_at)='integer' AND completed_at BETWEEN 0 AND 9007199254740991)),
 error_code TEXT CHECK(error_code IS NULL OR (typeof(error_code)='text' AND length(CAST(error_code AS BLOB)) BETWEEN 1 AND 64 AND instr(error_code,char(0))=0)),
 cancel_requested INTEGER NOT NULL DEFAULT 0 CHECK(typeof(cancel_requested)='integer' AND cancel_requested BETWEEN 0 AND 1),
 inventory_revision INTEGER NOT NULL DEFAULT 0 CHECK(typeof(inventory_revision)='integer' AND inventory_revision BETWEEN 0 AND 1),
 inventory_json TEXT CHECK(inventory_json IS NULL OR (typeof(inventory_json)='text' AND length(CAST(inventory_json AS BLOB)) BETWEEN 1 AND 16384 AND instr(inventory_json,char(0))=0)) CHECK(inventory_json IS NULL OR (json_valid(inventory_json) AND json_type(inventory_json)='object')),
 inventory_count INTEGER NOT NULL DEFAULT 0 CHECK(typeof(inventory_count)='integer' AND inventory_count BETWEEN 0 AND 20000),
 inventory_bytes INTEGER NOT NULL DEFAULT 0 CHECK(typeof(inventory_bytes)='integer' AND inventory_bytes BETWEEN 0 AND 67108864),
 outcome TEXT CHECK(outcome IN ('success','unsuccessful')),
 coverage_state TEXT NOT NULL DEFAULT 'untracked' CHECK(coverage_state IN ('untracked','pending','complete','incomplete')),
 cleanup_state TEXT NOT NULL DEFAULT 'not_needed' CHECK(cleanup_state IN ('pending','not_needed','complete','retained')),
 cleanup_json TEXT CHECK(cleanup_json IS NULL OR (typeof(cleanup_json)='text' AND length(CAST(cleanup_json AS BLOB)) BETWEEN 1 AND 4096 AND instr(cleanup_json,char(0))=0)) CHECK(cleanup_json IS NULL OR (json_valid(cleanup_json) AND json_type(cleanup_json)='object')),
 cleanup_resume_generation INTEGER NOT NULL DEFAULT 0 CHECK(typeof(cleanup_resume_generation)='integer' AND cleanup_resume_generation BETWEEN 0 AND 9007199254740991),
 cleanup_resume_requested INTEGER NOT NULL DEFAULT 0 CHECK(typeof(cleanup_resume_requested)='integer' AND cleanup_resume_requested BETWEEN 0 AND 1),
 CHECK((requested_provider_id IS NULL)=(requested_remote_id IS NULL)),
 CHECK(requested_provider_id IS NULL OR (length(requested_provider_id)=36 AND substr(requested_provider_id,9,1)='-' AND substr(requested_provider_id,14,1)='-' AND substr(requested_provider_id,19,1)='-' AND substr(requested_provider_id,24,1)='-' AND length(replace(requested_provider_id,'-',''))=32 AND replace(requested_provider_id,'-','') NOT GLOB '*[^0-9a-f]*')),
 CHECK(substr(source_path,1,1)='/'),
 CHECK(ready_order_at>=created_at),
 CHECK((status IN ('queued','running','retry_wait') AND completed_at IS NULL AND outcome IS NULL) OR (status IN ('succeeded','failed','cancelled') AND completed_at IS NOT NULL)),
 CHECK((status='succeeded')=(outcome IS NOT NULL)),
 CHECK(status!='queued' OR (attempts=0 AND started_at IS NULL AND run_generation=0)),
 CHECK(status NOT IN ('running','retry_wait','succeeded') OR (attempts>0 AND started_at IS NOT NULL AND run_generation>0)),
 CHECK(status!='retry_wait' OR attempts<3),
 CHECK((status IN ('failed','retry_wait') AND error_code IS NOT NULL) OR status NOT IN ('failed','retry_wait')),
 CHECK(status NOT IN ('succeeded','cancelled') OR error_code IS NULL),
 CHECK((candidate_id IS NULL AND tracked_json IS NULL AND processing_fence_json IS NULL AND predecessor_operation_id IS NULL AND coverage_state='untracked') OR (candidate_id IS NOT NULL AND tracked_json IS NOT NULL AND processing_fence_json IS NOT NULL AND coverage_state!='untracked')),
 CHECK(inventory_revision=0 OR inventory_json IS NOT NULL),
 CHECK(cleanup_state!='not_needed' OR cleanup_json IS NULL),
 CHECK(cleanup_state='not_needed' OR cleanup_json IS NOT NULL),
 CHECK(cleanup_json IS NULL OR ((json_type(cleanup_json,'$.version') IS 'integer' AND json_extract(cleanup_json,'$.version') IS 1) AND json_type(cleanup_json,'$.root_identity') IS 'object' AND json_type(cleanup_json,'$.parent_identity') IS 'object' AND json_type(cleanup_json,'$.root_identity.dev') IS 'integer' AND json_type(cleanup_json,'$.root_identity.ino') IS 'integer' AND json_type(cleanup_json,'$.parent_identity.dev') IS 'integer' AND json_type(cleanup_json,'$.parent_identity.ino') IS 'integer' AND json_type(cleanup_json,'$.reason') IS NOT NULL AND coalesce(json_extract(cleanup_json,'$.phase') IN ('observed','remove_intent','settled'),0))),
 CHECK(cleanup_state!='pending' OR coalesce(json_extract(cleanup_json,'$.phase') IN ('observed','remove_intent'),0)),
 CHECK(cleanup_state NOT IN ('complete','retained') OR json_extract(cleanup_json,'$.phase') IS 'settled'),
 CHECK(cleanup_state!='complete' OR json_type(cleanup_json,'$.reason') IS 'null'),
 CHECK(cleanup_state!='retained' OR coalesce(json_extract(cleanup_json,'$.reason') IN ('unimported_media','unknown_leftovers','archive_leftovers','identity_changed','path_denied','path_invalid','cleanup_io','cancelled'),0)),
 CHECK(cleanup_resume_requested=0 OR cleanup_state='pending')
);

CREATE TABLE downloaded_scan_children (
 id TEXT NOT NULL CHECK(typeof(id)='text' AND length(id)=36 AND substr(id,9,1)='-' AND substr(id,14,1)='-' AND substr(id,19,1)='-' AND substr(id,24,1)='-' AND length(replace(id,'-',''))=32 AND replace(id,'-','') NOT GLOB '*[^0-9a-f]*') PRIMARY KEY,
 command_id TEXT NOT NULL REFERENCES downloaded_scan_commands(id) ON DELETE RESTRICT,
 ordinal INTEGER NOT NULL CHECK(typeof(ordinal)='integer' AND ordinal BETWEEN 0 AND 19999),
 relative_path TEXT NOT NULL CHECK(typeof(relative_path)='text' AND length(CAST(relative_path AS BLOB)) BETWEEN 1 AND 4096 AND instr(relative_path,char(0))=0),
 source_path TEXT NOT NULL CHECK(typeof(source_path)='text' AND length(CAST(source_path AS BLOB)) BETWEEN 1 AND 4096 AND instr(source_path,char(0))=0),
 source_identity_json TEXT CHECK(source_identity_json IS NULL OR (typeof(source_identity_json)='text' AND length(CAST(source_identity_json AS BLOB)) BETWEEN 1 AND 4096 AND instr(source_identity_json,char(0))=0)) CHECK(source_identity_json IS NULL OR (json_valid(source_identity_json) AND json_type(source_identity_json)='object')),
 source_dev TEXT CHECK(source_dev IS NULL OR (typeof(source_dev)='text' AND length(CAST(source_dev AS BLOB)) BETWEEN 1 AND 20 AND instr(source_dev,char(0))=0)),
 source_ino TEXT CHECK(source_ino IS NULL OR (typeof(source_ino)='text' AND length(CAST(source_ino AS BLOB)) BETWEEN 1 AND 20 AND instr(source_ino,char(0))=0)),
 decision_json TEXT NOT NULL CHECK(typeof(decision_json)='text' AND length(CAST(decision_json AS BLOB)) BETWEEN 1 AND 16384 AND instr(decision_json,char(0))=0) CHECK(decision_json IS NULL OR (json_valid(decision_json) AND json_type(decision_json)='object')),
 selected INTEGER NOT NULL DEFAULT 0 CHECK(typeof(selected)='integer' AND selected BETWEEN 0 AND 1),
 series_id INTEGER REFERENCES series(id) ON DELETE RESTRICT,
 movie_id INTEGER REFERENCES movies(id) ON DELETE RESTRICT,
 expected_old_movie_file_id INTEGER REFERENCES movie_files(id) ON DELETE RESTRICT,
 destination TEXT CHECK(destination IS NULL OR (typeof(destination)='text' AND length(CAST(destination AS BLOB)) BETWEEN 1 AND 4096 AND instr(destination,char(0))=0)),
 resolved_mode TEXT CHECK(resolved_mode IN ('copy','move')),
 quality_id INTEGER,
 revision_json TEXT CHECK(revision_json IS NULL OR (typeof(revision_json)='text' AND length(CAST(revision_json AS BLOB))<=1024 AND json_valid(revision_json) AND json_type(revision_json)='object')),
 edition TEXT CHECK(edition IS NULL OR (typeof(edition)='text' AND length(CAST(edition AS BLOB)) BETWEEN 1 AND 1024 AND instr(edition,char(0))=0)),
 reasons_json TEXT NOT NULL DEFAULT '[]' CHECK(typeof(reasons_json)='text' AND length(CAST(reasons_json AS BLOB))<=8192 AND instr(reasons_json,char(0))=0 AND json_valid(reasons_json) AND json_type(reasons_json)='array' AND json_array_length(reasons_json)<=64),
 error_code TEXT CHECK(error_code IS NULL OR (typeof(error_code)='text' AND length(CAST(error_code AS BLOB)) BETWEEN 1 AND 64 AND instr(error_code,char(0))=0)),
 operation_id TEXT UNIQUE REFERENCES import_journal(operation_id) ON DELETE RESTRICT,
 disposition TEXT NOT NULL DEFAULT 'ready' CHECK(disposition IN ('ready','importing','imported','already_imported','rejected','failed','cancelled')),
 resume_generation INTEGER NOT NULL DEFAULT 0 CHECK(typeof(resume_generation)='integer' AND resume_generation BETWEEN 0 AND 9007199254740991),
 resume_requested INTEGER NOT NULL DEFAULT 0 CHECK(typeof(resume_requested)='integer' AND resume_requested BETWEEN 0 AND 1),
 -- Derived by the engine; callers cannot understate storage or desynchronize a counter.
 manifest_bytes INTEGER GENERATED ALWAYS AS (length(CAST(relative_path AS BLOB))+length(CAST(source_path AS BLOB))+coalesce(length(CAST(source_identity_json AS BLOB)),0)+length(CAST(decision_json AS BLOB))+length(CAST(reasons_json AS BLOB))+coalesce(length(CAST(destination AS BLOB)),0)+coalesce(length(CAST(revision_json AS BLOB)),0)+coalesce(length(CAST(edition AS BLOB)),0)+128) STORED,
 UNIQUE(command_id,ordinal),
 UNIQUE(command_id,relative_path),
 CHECK(substr(source_path,1,1)='/'),
 CHECK(relative_path!='..' AND substr(relative_path,1,1)!='/' AND relative_path NOT LIKE '../%' AND relative_path NOT LIKE '%/../%' AND relative_path NOT LIKE '%/..'),
 CHECK(series_id IS NULL OR movie_id IS NULL),
 CHECK(selected=0 OR ((series_id IS NOT NULL OR movie_id IS NOT NULL) AND destination IS NOT NULL AND resolved_mode IS NOT NULL AND quality_id IS NOT NULL AND revision_json IS NOT NULL AND source_identity_json IS NOT NULL AND source_dev IS NOT NULL AND source_ino IS NOT NULL)),
 CHECK(source_dev IS NULL OR (source_dev NOT GLOB '*[^0-9]*' AND (source_dev='0' OR substr(source_dev,1,1) BETWEEN '1' AND '9') AND (length(source_dev)<20 OR source_dev<='18446744073709551615'))),
 CHECK(source_ino IS NULL OR (source_ino NOT GLOB '*[^0-9]*' AND (source_ino='0' OR substr(source_ino,1,1) BETWEEN '1' AND '9') AND (length(source_ino)<20 OR source_ino<='18446744073709551615'))),
 CHECK(series_id IS NULL OR (edition IS NULL AND expected_old_movie_file_id IS NULL)),
 CHECK(disposition NOT IN ('importing','imported') OR operation_id IS NOT NULL),
 CHECK(resume_requested=0 OR operation_id IS NOT NULL)
);

CREATE TABLE downloaded_scan_child_episodes (
 child_id TEXT NOT NULL REFERENCES downloaded_scan_children(id) ON DELETE RESTRICT,
 command_id TEXT NOT NULL REFERENCES downloaded_scan_commands(id) ON DELETE RESTRICT,
 episode_id INTEGER NOT NULL REFERENCES episodes(id) ON DELETE RESTRICT,
 series_id INTEGER NOT NULL REFERENCES series(id) ON DELETE RESTRICT,
 expected_old_episode_file_id INTEGER REFERENCES episode_files(id) ON DELETE RESTRICT,
 PRIMARY KEY(child_id,episode_id),
 UNIQUE(command_id,episode_id)
);

CREATE TABLE scan_import_facts (
 operation_id TEXT PRIMARY KEY NOT NULL REFERENCES import_journal(operation_id) ON DELETE RESTRICT,
 child_id TEXT NOT NULL UNIQUE REFERENCES downloaded_scan_children(id) ON DELETE RESTRICT,
 command_id TEXT NOT NULL REFERENCES downloaded_scan_commands(id) ON DELETE RESTRICT,
 inventory_revision INTEGER NOT NULL CHECK(typeof(inventory_revision)='integer' AND inventory_revision BETWEEN 1 AND 1),
 run_generation INTEGER NOT NULL CHECK(typeof(run_generation)='integer' AND run_generation BETWEEN 1 AND 9007199254740991),
 quality_id INTEGER NOT NULL CHECK(typeof(quality_id)='integer' AND quality_id BETWEEN 0 AND 9007199254740991),
 revision_json TEXT NOT NULL CHECK(typeof(revision_json)='text' AND length(CAST(revision_json AS BLOB)) BETWEEN 1 AND 1024 AND instr(revision_json,char(0))=0) CHECK(revision_json IS NULL OR (json_valid(revision_json) AND json_type(revision_json)='object')),
 edition TEXT CHECK(edition IS NULL OR (typeof(edition)='text' AND length(CAST(edition AS BLOB)) BETWEEN 1 AND 1024 AND instr(edition,char(0))=0)),
 expected_size INTEGER NOT NULL CHECK(typeof(expected_size)='integer' AND expected_size BETWEEN 0 AND 9007199254740991),
 comparison_facts_json TEXT NOT NULL CHECK(typeof(comparison_facts_json)='text' AND length(CAST(comparison_facts_json AS BLOB)) BETWEEN 1 AND 16384 AND instr(comparison_facts_json,char(0))=0) CHECK(comparison_facts_json IS NULL OR (json_valid(comparison_facts_json) AND json_type(comparison_facts_json)='object'))
);

CREATE TABLE scan_import_replacements (
 operation_id TEXT NOT NULL REFERENCES scan_import_facts(operation_id) ON DELETE RESTRICT,
 media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
 old_file_id INTEGER NOT NULL CHECK(typeof(old_file_id)='integer' AND old_file_id BETWEEN 1 AND 9007199254740991),
 old_episode_file_id INTEGER REFERENCES episode_files(id) ON DELETE RESTRICT,
 old_movie_file_id INTEGER REFERENCES movie_files(id) ON DELETE RESTRICT,
 old_file_json TEXT NOT NULL CHECK(typeof(old_file_json)='text' AND length(CAST(old_file_json AS BLOB)) BETWEEN 1 AND 131072 AND instr(old_file_json,char(0))=0) CHECK(old_file_json IS NULL OR (json_valid(old_file_json) AND json_type(old_file_json)='object')),
 retirement_state TEXT NOT NULL DEFAULT 'pending' CHECK(retirement_state IN ('pending','quarantined','shared_retained')),
 retirement_json TEXT CHECK(retirement_json IS NULL OR (typeof(retirement_json)='text' AND length(CAST(retirement_json AS BLOB)) BETWEEN 1 AND 4096 AND instr(retirement_json,char(0))=0)) CHECK(retirement_json IS NULL OR (json_valid(retirement_json) AND json_type(retirement_json)='object')),
 PRIMARY KEY(operation_id,media_type,old_file_id),
 CHECK((media_type='tv' AND old_episode_file_id=old_file_id AND old_episode_file_id IS NOT NULL AND old_movie_file_id IS NULL) OR (media_type='movies' AND old_movie_file_id=old_file_id AND old_movie_file_id IS NOT NULL AND old_episode_file_id IS NULL)),
 CHECK(retirement_state!='quarantined' OR retirement_json IS NOT NULL),
 CHECK(retirement_json IS NULL OR json_type(retirement_json,'$.directory') IS 'object')
);

CREATE TABLE import_history_episodes (
 operation_id TEXT NOT NULL REFERENCES import_history(operation_id) ON DELETE RESTRICT,
 episode_id INTEGER NOT NULL REFERENCES episodes(id) ON DELETE RESTRICT,
 episode_file_id INTEGER NOT NULL CHECK(typeof(episode_file_id)='integer' AND episode_file_id BETWEEN 1 AND 9007199254740991),
 PRIMARY KEY(operation_id,episode_id)
);

CREATE TABLE downloaded_scan_coverage (
 command_id TEXT NOT NULL REFERENCES downloaded_scan_commands(id) ON DELETE RESTRICT,
 episode_id INTEGER REFERENCES episodes(id) ON DELETE RESTRICT,
 movie_id INTEGER REFERENCES movies(id) ON DELETE RESTRICT,
 credited_operation_id TEXT REFERENCES import_history(operation_id) ON DELETE RESTRICT,
 credited_file_id INTEGER CHECK(credited_file_id IS NULL OR (typeof(credited_file_id)='integer' AND credited_file_id>0)),
 CHECK((episode_id IS NULL)!=(movie_id IS NULL)),
 CHECK((credited_operation_id IS NULL)=(credited_file_id IS NULL))
);

CREATE INDEX downloaded_scan_ready ON downloaded_scan_commands(status,next_attempt_at,priority,ready_order_at,id);

CREATE INDEX downloaded_scan_history ON downloaded_scan_commands(media_type,created_at,id);

CREATE INDEX downloaded_scan_candidate ON downloaded_scan_commands(candidate_id,status,id);

CREATE INDEX downloaded_scan_successor ON downloaded_scan_commands(supersedes_command_id);

CREATE INDEX downloaded_scan_child_state ON downloaded_scan_children(command_id,disposition,ordinal);

CREATE INDEX downloaded_scan_destination ON downloaded_scan_children(destination) WHERE selected=1;

CREATE INDEX downloaded_scan_source ON downloaded_scan_children(source_path) WHERE selected=1;

CREATE INDEX downloaded_scan_episode ON downloaded_scan_child_episodes(episode_id,child_id);

CREATE INDEX downloaded_scan_old_episode ON downloaded_scan_child_episodes(expected_old_episode_file_id,child_id);

CREATE INDEX scan_replaced_episode ON scan_import_replacements(old_episode_file_id,retirement_state);

CREATE INDEX scan_replaced_movie ON scan_import_replacements(old_movie_file_id,retirement_state);

CREATE INDEX import_history_member ON import_history_episodes(episode_id,operation_id);

CREATE INDEX downloaded_scan_credit ON downloaded_scan_coverage(credited_operation_id);

CREATE UNIQUE INDEX downloaded_scan_active_candidate ON downloaded_scan_commands(candidate_id) WHERE candidate_id IS NOT NULL AND status IN ('queued','running','retry_wait');

CREATE UNIQUE INDEX downloaded_scan_inode ON downloaded_scan_children(command_id,source_dev,source_ino) WHERE selected=1;

CREATE UNIQUE INDEX downloaded_scan_expected_episode ON downloaded_scan_coverage(command_id,episode_id) WHERE episode_id IS NOT NULL;

CREATE UNIQUE INDEX downloaded_scan_expected_movie ON downloaded_scan_coverage(command_id,movie_id) WHERE movie_id IS NOT NULL;

-- Exact legacy pool expressions: only the tenth count is appended, all other bytes retained.

DROP TRIGGER commands_admit;
CREATE TRIGGER commands_admit BEFORE INSERT ON commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.provider_id AND p.revision=NEW.provider_revision AND p.enabled=1 AND p.implementation='qbittorrent' AND s.media_type=NEW.media_type) THEN RAISE(ABORT,'invalid refresh provider scope or revision') END;
END;

DROP TRIGGER metadata_refresh_admit;
CREATE TRIGGER metadata_refresh_admit BEFORE INSERT ON metadata_refresh_commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM series s WHERE s.id=NEW.series_id AND s.tvdb_id=NEW.external_id) AND NOT EXISTS(SELECT 1 FROM movies m JOIN movie_metadata d ON d.id=m.metadata_id WHERE m.id=NEW.movie_id AND m.metadata_id=NEW.metadata_id AND d.tmdb_id=NEW.external_id) THEN RAISE(ABORT,'invalid metadata refresh target') END;
END;

DROP TRIGGER blocklist_clear_admit;
CREATE TRIGGER blocklist_clear_admit BEFORE INSERT ON blocklist_clear_commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
END;

DROP TRIGGER rss_commands_admit;
CREATE TRIGGER rss_commands_admit BEFORE INSERT ON rss_commands BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 OR NEW.fetch_complete!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT (EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.indexer_id AND p.revision=NEW.indexer_revision AND p.implementation IN ('torznab','newznab') AND s.media_type=NEW.media_type AND p.enabled=1) AND (NEW.selection_kind='default' OR EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.client_id AND p.revision=NEW.client_revision AND p.implementation='qbittorrent' AND s.media_type=NEW.media_type AND p.enabled=1))) THEN RAISE(ABORT,'invalid RSS provider scope or revision') END;
END;

DROP TRIGGER search_commands_admit;
CREATE TRIGGER search_commands_admit BEFORE INSERT ON search_commands BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 OR NEW.fetch_complete!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT (EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.indexer_id AND p.revision=NEW.indexer_revision AND p.implementation IN ('torznab','newznab') AND s.media_type=NEW.media_type AND p.enabled=1) AND (NEW.selection_kind='default' OR EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.client_id AND p.revision=NEW.client_revision AND p.implementation='qbittorrent' AND s.media_type=NEW.media_type AND p.enabled=1))) THEN RAISE(ABORT,'invalid RSS provider scope or revision') END;
 SELECT CASE WHEN NOT ((NEW.media_type='tv' AND EXISTS(SELECT 1 FROM episodes e WHERE e.id=NEW.requested_episode_id AND json_extract(NEW.captured_target_json,'$.media_type')='tv' AND json_extract(NEW.captured_target_json,'$.episode_id')=e.id AND json_extract(NEW.captured_target_json,'$.series_id')=e.series_id)) OR (NEW.media_type='movies' AND EXISTS(SELECT 1 FROM movies m WHERE m.id=NEW.requested_movie_id AND json_extract(NEW.captured_target_json,'$.media_type')='movies' AND json_extract(NEW.captured_target_json,'$.movie_id')=m.id AND json_extract(NEW.captured_target_json,'$.metadata_id')=m.metadata_id))) THEN RAISE(ABORT,'invalid search target') END;
END;

DROP TRIGGER manual_import_commands_admit;
CREATE TRIGGER manual_import_commands_admit BEFORE INSERT ON manual_import_commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM import_journal WHERE operation_id=NEW.operation_id) THEN RAISE(ABORT,'operation has no import journal') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM rss_candidate_imports WHERE operation_id=NEW.operation_id) THEN RAISE(ABORT,'operation is owned by automated download import') END;
END;

DROP TRIGGER quality_reset_admit;
CREATE TRIGGER quality_reset_admit BEFORE INSERT ON quality_reset_commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
END;

DROP TRIGGER rescan_admit;
CREATE TRIGGER rescan_admit BEFORE INSERT ON rescan_commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT ((NEW.media_type='tv' AND EXISTS(SELECT 1 FROM series WHERE id=NEW.series_id)) OR (NEW.media_type='movies' AND EXISTS(SELECT 1 FROM movies WHERE id=NEW.movie_id))) THEN RAISE(ABORT,'invalid rescan target') END;
 -- 'preview' does not touch the filesystem yet and may never be executed, so it alone must not
 -- block a rescan forever; 'complete' is done. The window that matters is real transfer work.
 SELECT CASE WHEN EXISTS(SELECT 1 FROM import_journal j JOIN operations o ON o.id=j.operation_id WHERE j.phase NOT IN ('preview','complete') AND ((NEW.media_type='tv' AND o.media_type='episode' AND EXISTS(SELECT 1 FROM episodes e WHERE e.id=o.episode_id AND e.series_id=NEW.series_id)) OR (NEW.media_type='movies' AND o.media_type='movie' AND o.movie_id=NEW.movie_id))) THEN RAISE(ABORT,'rescan target has an in-flight import') END;
 -- 'queued' has not started preflight/transfer work; 'blocked'/'cancelled' are paused, not active.
 SELECT CASE WHEN EXISTS(SELECT 1 FROM download_processing dp JOIN rss_candidates r ON r.id=dp.candidate_id WHERE dp.status IN ('checking','importing') AND ((NEW.media_type='tv' AND r.media_type='tv' AND r.series_id=NEW.series_id) OR (NEW.media_type='movies' AND r.media_type='movies' AND r.movie_id=NEW.movie_id))) THEN RAISE(ABORT,'rescan target has in-flight download processing') END;
END;

DROP TRIGGER health_admit;
CREATE TRIGGER health_admit BEFORE INSERT ON health_commands BEGIN SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END; SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 OR NEW.epoch IS NOT (SELECT epoch FROM health_lifecycle WHERE id=1) THEN RAISE(ABORT,'invalid health admission') END; END;

CREATE TEMP TABLE _scan54_processing AS SELECT * FROM download_processing;
DROP TABLE download_processing;
CREATE TABLE download_processing (
 candidate_id TEXT PRIMARY KEY NOT NULL REFERENCES rss_candidates(id) ON DELETE RESTRICT,
 policy_revision INTEGER NOT NULL CHECK(typeof(policy_revision)='integer' AND policy_revision BETWEEN 1 AND 9007199254740991),
 status TEXT NOT NULL CHECK(status IN ('queued','checking','importing','imported','blocked','cancelled')),
 resume_requested INTEGER NOT NULL DEFAULT 0 CHECK(typeof(resume_requested)='integer' AND resume_requested IN (0,1)),
 preflight_attempts INTEGER NOT NULL DEFAULT 0 CHECK(typeof(preflight_attempts)='integer' AND preflight_attempts BETWEEN 0 AND 3),
 total_preflight_attempts INTEGER NOT NULL DEFAULT 0 CHECK(typeof(total_preflight_attempts)='integer' AND total_preflight_attempts BETWEEN 0 AND 9007199254740991),
 next_attempt_at INTEGER NOT NULL CHECK(typeof(next_attempt_at)='integer' AND next_attempt_at BETWEEN 0 AND 9007199254740991),
 created_at INTEGER NOT NULL CHECK(typeof(created_at)='integer' AND created_at BETWEEN 0 AND 9007199254740991),
 updated_at INTEGER NOT NULL CHECK(typeof(updated_at)='integer' AND updated_at BETWEEN 0 AND 9007199254740991),
 error_code TEXT CHECK(error_code IS NULL OR error_code IN ('interrupted','storage_error','provider_changed','processing_disabled','download_not_complete','download_failed','download_unavailable','download_timeout','path_mapping_missing','path_mapping_changed','invalid_download_files','ambiguous_files','source_unavailable','target_changed','unsupported_download','quality_rejected','import_busy','import_failed','import_conflict')),
 reasons_json TEXT NOT NULL DEFAULT '[]' CHECK(length(CAST(reasons_json AS BLOB))<=8192 AND json_valid(reasons_json) AND json_type(reasons_json)='array' AND json_array_length(reasons_json)<=64),
 scan_command_id TEXT REFERENCES downloaded_scan_commands(id) ON DELETE RESTRICT,
 CHECK(status='importing' OR resume_requested=0),
 CHECK(total_preflight_attempts>=preflight_attempts),
 CHECK(status!='queued' OR preflight_attempts<3),
 CHECK(status NOT IN ('checking','importing','imported') OR preflight_attempts>0 OR (status='imported' AND scan_command_id IS NOT NULL)),
 CHECK(status!='imported' OR error_code IS NULL),
 CHECK(status!='blocked' OR error_code IS NOT NULL)
);

INSERT INTO download_processing(candidate_id,policy_revision,status,resume_requested,preflight_attempts,total_preflight_attempts,next_attempt_at,created_at,updated_at,error_code,reasons_json,scan_command_id) SELECT candidate_id,policy_revision,status,resume_requested,preflight_attempts,total_preflight_attempts,next_attempt_at,created_at,updated_at,error_code,reasons_json,NULL FROM _scan54_processing;
DROP TABLE _scan54_processing;

CREATE INDEX download_processing_path_unresolved ON download_processing(candidate_id)
WHERE status NOT IN ('imported','cancelled');

CREATE INDEX download_processing_ready ON download_processing(status,next_attempt_at,created_at,candidate_id);

CREATE TRIGGER processing_admit BEFORE INSERT ON download_processing BEGIN
 SELECT CASE WHEN NEW.status!='queued' OR NEW.preflight_attempts!=0 OR NEW.total_preflight_attempts!=0 OR NEW.resume_requested!=0 THEN RAISE(ABORT,'processing must be queued') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM rss_candidates r JOIN download_processing_policies p ON p.provider_id=r.client_id AND p.media_type=r.media_type JOIN providers v ON v.id=p.provider_id JOIN provider_scopes s ON s.provider_id=v.id AND s.media_type=r.media_type WHERE r.id=NEW.candidate_id AND r.status='observed' AND p.enabled=1 AND v.enabled=1 AND v.implementation='qbittorrent' AND p.revision=NEW.policy_revision AND p.provider_revision=r.client_revision AND v.revision=r.client_revision) THEN RAISE(ABORT,'processing requires current authorized observed receipt') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM json_each(NEW.reasons_json) WHERE type!='text' OR length(value)>128 OR value GLOB '*[^a-z0-9_]*') THEN RAISE(ABORT,'invalid processing reason') END;
END;

CREATE TRIGGER processing_retained BEFORE DELETE ON download_processing BEGIN SELECT RAISE(ABORT,'download processing journal must be retained'); END;

CREATE TRIGGER processing_transition BEFORE UPDATE ON download_processing WHEN NEW.scan_command_id IS NULL BEGIN
 SELECT CASE WHEN NEW.candidate_id IS NOT OLD.candidate_id OR NEW.created_at IS NOT OLD.created_at THEN RAISE(ABORT,'processing identity is immutable') END;
 SELECT CASE WHEN NOT ((OLD.status='queued' AND NEW.status IN ('checking','blocked','cancelled')) OR (OLD.status='checking' AND NEW.status IN ('queued','importing','blocked','cancelled')) OR (OLD.status='importing' AND NEW.status IN ('importing','imported')) OR (OLD.status IN ('blocked','cancelled') AND NEW.status IN ('queued','cancelled'))) THEN RAISE(ABORT,'invalid processing transition') END;
 SELECT CASE WHEN NEW.total_preflight_attempts!=OLD.total_preflight_attempts+CASE WHEN OLD.status='queued' AND NEW.status='checking' THEN 1 ELSE 0 END OR NEW.preflight_attempts!=CASE WHEN OLD.status='queued' AND NEW.status='checking' THEN OLD.preflight_attempts+1 WHEN OLD.status IN ('blocked','cancelled') AND NEW.status='queued' THEN 0 ELSE OLD.preflight_attempts END THEN RAISE(ABORT,'invalid processing attempt counter') END;
 SELECT CASE WHEN NEW.policy_revision IS NOT OLD.policy_revision AND NOT(OLD.status IN ('blocked','cancelled') AND NEW.status='queued') THEN RAISE(ABORT,'processing authorization is immutable within retry round') END;
 SELECT CASE WHEN NEW.status IN ('checking','queued') AND NOT EXISTS(SELECT 1 FROM rss_candidates r JOIN download_processing_policies p ON p.provider_id=r.client_id AND p.media_type=r.media_type JOIN providers v ON v.id=p.provider_id JOIN provider_scopes s ON s.provider_id=v.id AND s.media_type=r.media_type WHERE r.id=NEW.candidate_id AND r.status='observed' AND p.enabled=1 AND v.enabled=1 AND p.revision=NEW.policy_revision AND p.provider_revision=r.client_revision AND v.revision=r.client_revision) THEN RAISE(ABORT,'processing authorization changed') END;
 SELECT CASE WHEN NEW.status IN ('queued','checking','blocked','cancelled') AND EXISTS(SELECT 1 FROM rss_candidate_imports WHERE candidate_id=NEW.candidate_id) THEN RAISE(ABORT,'linked imports must resume same operation') END;
 SELECT CASE WHEN NEW.status IN ('importing','imported') AND NOT EXISTS(SELECT 1 FROM rss_candidate_imports WHERE candidate_id=NEW.candidate_id) THEN RAISE(ABORT,'processing requires linked import') END;
 SELECT CASE WHEN NEW.status='imported' AND NOT EXISTS(SELECT 1 FROM rss_candidate_imports i JOIN import_journal j ON j.operation_id=i.operation_id JOIN import_history h ON h.operation_id=j.operation_id WHERE i.candidate_id=NEW.candidate_id AND j.phase='complete') THEN RAISE(ABORT,'processing requires complete import history') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM json_each(NEW.reasons_json) WHERE type!='text' OR length(value)>128 OR value GLOB '*[^a-z0-9_]*') THEN RAISE(ABORT,'invalid processing reason') END;
END;

CREATE TRIGGER rescan_blocks_processing BEFORE UPDATE OF status ON download_processing
WHEN NEW.status='checking' AND OLD.status!='checking' AND EXISTS(SELECT 1 FROM rescan_commands r JOIN rss_candidates c ON c.id=NEW.candidate_id WHERE r.status IN ('queued','running','retry_wait') AND ((c.media_type='tv' AND r.media_type='tv' AND r.series_id=c.series_id) OR (c.media_type='movies' AND r.media_type='movies' AND r.movie_id=c.movie_id)))
BEGIN SELECT RAISE(ABORT,'download processing target has an active rescan'); END;

CREATE VIEW downloaded_scan_reserved_children AS
SELECT c.id,c.command_id,c.series_id,c.movie_id,c.source_path,c.source_dev,c.source_ino,c.destination,c.operation_id
FROM downloaded_scan_children c JOIN downloaded_scan_commands p ON p.id=c.command_id
LEFT JOIN import_journal j ON j.operation_id=c.operation_id
WHERE c.selected=1 AND (p.status IN ('queued','running','retry_wait') OR
 (j.phase!='complete' AND NOT(c.disposition='cancelled' AND j.phase='preview' AND j.path_failure_code IS NULL AND j.error_code IS NULL)));
CREATE INDEX downloaded_scan_child_bytes ON downloaded_scan_children(command_id,manifest_bytes);
CREATE INDEX downloaded_scan_coverage_parent ON downloaded_scan_coverage(command_id);
CREATE VIEW downloaded_scan_inventory_size AS
SELECT p.id,
 (SELECT count(*) FROM downloaded_scan_children c WHERE c.command_id=p.id) AS child_count,
 (SELECT count(*) FROM downloaded_scan_child_episodes m WHERE m.command_id=p.id) AS member_count,
 (SELECT count(*) FROM downloaded_scan_coverage v WHERE v.command_id=p.id) AS expected_count,
 coalesce((SELECT sum(c.manifest_bytes) FROM downloaded_scan_children c WHERE c.command_id=p.id),0)
 +64*(SELECT count(*) FROM downloaded_scan_child_episodes m WHERE m.command_id=p.id)
 +64*(SELECT count(*) FROM downloaded_scan_coverage v WHERE v.command_id=p.id) AS bytes
FROM downloaded_scan_commands p;
CREATE VIEW downloaded_scan_complete_coverage AS
SELECT p.id,p.candidate_id FROM downloaded_scan_commands p
WHERE p.inventory_revision=1 AND p.coverage_state='complete' AND p.status IN ('succeeded','cancelled','failed')
 AND EXISTS(SELECT 1 FROM downloaded_scan_coverage v WHERE v.command_id=p.id)
 AND NOT EXISTS(SELECT 1 FROM downloaded_scan_coverage v LEFT JOIN import_journal j ON j.operation_id=v.credited_operation_id WHERE v.command_id=p.id AND (v.credited_operation_id IS NULL OR j.phase IS NOT 'complete'));

CREATE TRIGGER downloaded_scan_admit BEFORE INSERT ON downloaded_scan_commands BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 OR NEW.run_generation!=0 OR NEW.cancel_requested!=0 OR NEW.inventory_revision!=0 OR NEW.inventory_count!=0 OR NEW.inventory_bytes!=0 OR NEW.inventory_json IS NOT NULL OR NEW.candidate_id IS NOT NULL OR NEW.cleanup_json IS NOT NULL OR NEW.cleanup_resume_generation!=0 OR NEW.cleanup_resume_requested!=0 OR NEW.ready_order_at!=NEW.created_at THEN RAISE(ABORT,'scan must be queued without resolved authority') END;
 SELECT CASE WHEN NEW.supersedes_command_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM downloaded_scan_commands p WHERE p.id=NEW.supersedes_command_id AND p.media_type=NEW.media_type AND p.source_path=NEW.source_path AND p.status IN ('succeeded','failed','cancelled') AND p.coverage_state='incomplete' AND NOT EXISTS(SELECT 1 FROM downloaded_scan_reserved_children c WHERE c.command_id=p.id)) THEN RAISE(ABORT,'scan successor requires settled incomplete predecessor') END;
END;

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
 SELECT CASE WHEN NEW.inventory_revision!=OLD.inventory_revision AND (NEW.status!='running' OR NEW.cancel_requested!=0 OR (json_type(NEW.inventory_json,'$.version') IS NOT 'integer' OR json_extract(NEW.inventory_json,'$.version') IS NOT 1) OR NOT EXISTS(SELECT 1 FROM downloaded_scan_inventory_size s WHERE s.id=NEW.id AND NEW.inventory_count=s.child_count AND NEW.inventory_bytes=s.bytes AND s.member_count<=20000 AND s.expected_count<=20000 AND s.bytes<=67108864)) THEN RAISE(ABORT,'scan inventory must publish completely') END;
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

CREATE TRIGGER downloaded_scan_commands_retained BEFORE DELETE ON downloaded_scan_commands
WHEN 1
BEGIN SELECT RAISE(ABORT,'scan authority must be retained'); END;

CREATE TRIGGER downloaded_scan_children_retained BEFORE DELETE ON downloaded_scan_children
WHEN 1
BEGIN SELECT RAISE(ABORT,'scan authority must be retained'); END;

CREATE TRIGGER downloaded_scan_child_episodes_retained BEFORE DELETE ON downloaded_scan_child_episodes
WHEN 1
BEGIN SELECT RAISE(ABORT,'scan authority must be retained'); END;

CREATE TRIGGER scan_import_facts_retained BEFORE DELETE ON scan_import_facts
WHEN 1
BEGIN SELECT RAISE(ABORT,'scan authority must be retained'); END;

CREATE TRIGGER scan_import_replacements_retained BEFORE DELETE ON scan_import_replacements
WHEN 1
BEGIN SELECT RAISE(ABORT,'scan authority must be retained'); END;

CREATE TRIGGER import_history_episodes_retained BEFORE DELETE ON import_history_episodes
WHEN 1
BEGIN SELECT RAISE(ABORT,'scan authority must be retained'); END;

CREATE TRIGGER downloaded_scan_coverage_retained BEFORE DELETE ON downloaded_scan_coverage
WHEN 1
BEGIN SELECT RAISE(ABORT,'scan authority must be retained'); END;

CREATE TRIGGER downloaded_scan_child_admit BEFORE INSERT ON downloaded_scan_children BEGIN
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM downloaded_scan_commands p WHERE p.id=NEW.command_id AND p.status='running' AND p.inventory_revision=0 AND p.cancel_requested=0 AND ((p.media_type='tv' AND NEW.movie_id IS NULL) OR (p.media_type='movies' AND NEW.series_id IS NULL)) AND (NEW.source_path=p.source_path OR substr(NEW.source_path,1,length(rtrim(p.source_path,'/'))+1)=rtrim(p.source_path,'/')||'/')) THEN RAISE(ABORT,'child requires unpublished scoped inventory') END;
 SELECT CASE WHEN NEW.operation_id IS NOT NULL OR NEW.resume_generation!=0 OR NEW.resume_requested!=0 OR NEW.disposition NOT IN ('ready','rejected','already_imported') OR (NEW.selected=1 AND NEW.disposition!='ready') OR (NEW.selected=0 AND NEW.disposition='ready') THEN RAISE(ABORT,'invalid initial child disposition') END;
 SELECT CASE WHEN (json_type(NEW.decision_json,'$.version') IS NOT 'integer' OR json_extract(NEW.decision_json,'$.version') IS NOT 1) OR (NEW.source_identity_json IS NOT NULL AND ((json_type(NEW.source_identity_json,'$.version') IS NOT 'integer' OR json_extract(NEW.source_identity_json,'$.version') IS NOT 1) OR json_type(NEW.source_identity_json,'$.dev') IS NOT 'integer' OR json_type(NEW.source_identity_json,'$.ino') IS NOT 'integer')) THEN RAISE(ABORT,'invalid scan evidence version') END;
 SELECT CASE WHEN NEW.source_dev IS NOT NULL AND ((NEW.source_identity_json -> '$.dev') IS NOT NEW.source_dev OR (NEW.source_identity_json -> '$.ino') IS NOT NEW.source_ino) THEN RAISE(ABORT,'scan inode key differs from evidence') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM json_each(NEW.reasons_json) WHERE type!='text' OR length(value)>128 OR value GLOB '*[^a-z0-9_]*') OR (SELECT count(*) FROM json_each(NEW.reasons_json))!=(SELECT count(DISTINCT value) FROM json_each(NEW.reasons_json)) THEN RAISE(ABORT,'invalid scan reason') END;
 SELECT CASE WHEN NEW.selected=1 AND EXISTS(SELECT 1 FROM downloaded_scan_reserved_children c WHERE c.command_id!=NEW.command_id AND (c.destination=NEW.destination OR c.source_path=NEW.source_path OR (c.source_dev=NEW.source_dev AND c.source_ino=NEW.source_ino) OR (NEW.movie_id IS NOT NULL AND c.movie_id=NEW.movie_id))) THEN RAISE(ABORT,'scan path or movie already reserved') END;
 SELECT CASE WHEN NEW.selected=1 AND EXISTS(SELECT 1 FROM rescan_commands r WHERE r.status IN ('queued','running','retry_wait') AND ((NEW.series_id IS NOT NULL AND r.series_id=NEW.series_id) OR (NEW.movie_id IS NOT NULL AND r.movie_id=NEW.movie_id))) THEN RAISE(ABORT,'scan owner has active rescan') END;
 SELECT CASE WHEN NEW.selected=1 AND NEW.movie_id IS NOT NULL AND EXISTS(SELECT 1 FROM rss_candidates r JOIN downloaded_scan_commands p ON p.id=NEW.command_id WHERE r.movie_id=NEW.movie_id AND r.status IN ('prepared','submitting','reconciling','observed','needs_attention') AND r.id IS NOT p.candidate_id AND NOT EXISTS(SELECT 1 FROM rss_candidate_imports i JOIN import_journal j ON j.operation_id=i.operation_id WHERE i.candidate_id=r.id AND j.phase='complete') AND NOT EXISTS(SELECT 1 FROM downloaded_scan_complete_coverage v WHERE v.candidate_id=r.id)) THEN RAISE(ABORT,'scan movie has unrelated receipt owner') END;
END;

CREATE TRIGGER downloaded_scan_member_admit BEFORE INSERT ON downloaded_scan_child_episodes BEGIN
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM downloaded_scan_children c JOIN downloaded_scan_commands p ON p.id=c.command_id JOIN episodes e ON e.id=NEW.episode_id WHERE c.id=NEW.child_id AND c.command_id=NEW.command_id AND c.selected=1 AND c.series_id=NEW.series_id AND e.series_id=NEW.series_id AND e.episode_file_id IS NEW.expected_old_episode_file_id AND p.status='running' AND p.inventory_revision=0 AND p.cancel_requested=0) THEN RAISE(ABORT,'invalid scan episode membership') END;
 SELECT CASE WHEN (SELECT count(*) FROM downloaded_scan_child_episodes WHERE command_id=NEW.command_id)>=20000 THEN RAISE(ABORT,'scan membership capacity reached') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM downloaded_scan_child_episodes m JOIN downloaded_scan_reserved_children c ON c.id=m.child_id WHERE m.episode_id=NEW.episode_id AND m.command_id!=NEW.command_id) THEN RAISE(ABORT,'scan episode already reserved') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM rss_candidate_episodes e JOIN rss_candidates r ON r.id=e.candidate_id JOIN downloaded_scan_commands p ON p.id=NEW.command_id WHERE e.episode_id=NEW.episode_id AND r.status IN ('prepared','submitting','reconciling','observed','needs_attention') AND r.id IS NOT p.candidate_id AND NOT EXISTS(SELECT 1 FROM rss_candidate_imports i JOIN import_journal j ON j.operation_id=i.operation_id WHERE i.candidate_id=r.id AND j.phase='complete') AND NOT EXISTS(SELECT 1 FROM downloaded_scan_complete_coverage v WHERE v.candidate_id=r.id)) THEN RAISE(ABORT,'scan episode has unrelated receipt owner') END;
END;

CREATE TRIGGER downloaded_scan_member_immutable BEFORE UPDATE ON downloaded_scan_child_episodes
WHEN 1
BEGIN SELECT RAISE(ABORT,'scan membership is immutable'); END;

CREATE TRIGGER downloaded_scan_child_transition BEFORE UPDATE ON downloaded_scan_children BEGIN
 SELECT CASE WHEN NEW.id IS NOT OLD.id OR NEW.command_id IS NOT OLD.command_id OR NEW.ordinal IS NOT OLD.ordinal OR NEW.relative_path IS NOT OLD.relative_path OR NEW.source_path IS NOT OLD.source_path OR NEW.source_identity_json IS NOT OLD.source_identity_json OR NEW.source_dev IS NOT OLD.source_dev OR NEW.source_ino IS NOT OLD.source_ino OR NEW.decision_json IS NOT OLD.decision_json OR NEW.selected IS NOT OLD.selected OR NEW.series_id IS NOT OLD.series_id OR NEW.movie_id IS NOT OLD.movie_id OR NEW.expected_old_movie_file_id IS NOT OLD.expected_old_movie_file_id OR NEW.destination IS NOT OLD.destination OR NEW.resolved_mode IS NOT OLD.resolved_mode OR NEW.quality_id IS NOT OLD.quality_id OR NEW.revision_json IS NOT OLD.revision_json OR NEW.edition IS NOT OLD.edition THEN RAISE(ABORT,'scan inventory facts are immutable') END;
 SELECT CASE WHEN OLD.operation_id IS NOT NULL AND NEW.operation_id IS NOT OLD.operation_id THEN RAISE(ABORT,'scan operation is immutable') END;
 SELECT CASE WHEN NEW.operation_id IS NOT OLD.operation_id AND NOT EXISTS(SELECT 1 FROM scan_import_facts f JOIN downloaded_scan_commands p ON p.id=f.command_id WHERE f.child_id=NEW.id AND f.operation_id=NEW.operation_id AND p.status='running' AND p.cancel_requested=0 AND p.inventory_revision=1 AND p.run_generation=f.run_generation) THEN RAISE(ABORT,'scan link requires current producer authority') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM downloaded_scan_commands p WHERE p.id=NEW.command_id AND p.status='running' AND p.inventory_revision=1) THEN RAISE(ABORT,'scan result requires active published inventory') END;
 SELECT CASE WHEN OLD.disposition IN ('imported','already_imported','rejected','cancelled') THEN RAISE(ABORT,'terminal child is immutable') END;
 SELECT CASE WHEN NOT(NEW.disposition=OLD.disposition OR (OLD.disposition='ready' AND NEW.disposition IN ('importing','failed','cancelled')) OR (OLD.disposition='importing' AND NEW.disposition IN ('imported','failed')) OR (OLD.disposition='failed' AND NEW.disposition IN ('importing','cancelled'))) THEN RAISE(ABORT,'invalid child transition') END;
 SELECT CASE WHEN NEW.disposition='imported' AND NOT EXISTS(SELECT 1 FROM import_journal j JOIN import_history h ON h.operation_id=j.operation_id WHERE j.operation_id=NEW.operation_id AND j.phase='complete') THEN RAISE(ABORT,'child completion requires history') END;
 SELECT CASE WHEN NEW.disposition='cancelled' AND NEW.operation_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM import_journal j WHERE j.operation_id=NEW.operation_id AND j.phase='preview' AND j.path_failure_code IS NULL AND j.error_code IS NULL) THEN RAISE(ABORT,'admitted child cannot be cancelled') END;
 SELECT CASE WHEN NEW.resume_generation NOT IN (OLD.resume_generation,OLD.resume_generation+1) OR (NEW.resume_generation=OLD.resume_generation+1 AND (OLD.resume_requested!=0 OR NEW.resume_requested!=1 OR NOT EXISTS(SELECT 1 FROM import_journal j WHERE j.operation_id=NEW.operation_id AND j.phase!='complete' AND (j.path_failure_code IS NOT NULL OR j.error_code IS NOT NULL)))) OR (NEW.resume_requested>OLD.resume_requested AND NEW.resume_generation!=OLD.resume_generation+1) THEN RAISE(ABORT,'invalid scan resume generation') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM json_each(NEW.reasons_json) WHERE type!='text' OR length(value)>128 OR value GLOB '*[^a-z0-9_]*') OR (SELECT count(*) FROM json_each(NEW.reasons_json))!=(SELECT count(DISTINCT value) FROM json_each(NEW.reasons_json)) THEN RAISE(ABORT,'invalid scan reason') END;
END;

-- Producer facts are admitted before the child link, in the same immediate transaction.
CREATE TRIGGER scan_import_facts_admit BEFORE INSERT ON scan_import_facts BEGIN
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM downloaded_scan_children c JOIN downloaded_scan_commands p ON p.id=c.command_id JOIN operations o ON o.id=NEW.operation_id JOIN import_journal j ON j.operation_id=o.id JOIN quality_definitions q ON q.media_type=p.media_type AND q.quality_id=NEW.quality_id WHERE c.id=NEW.child_id AND c.command_id=NEW.command_id AND c.selected=1 AND c.disposition='ready' AND c.operation_id IS NULL AND p.status='running' AND p.cancel_requested=0 AND p.inventory_revision=NEW.inventory_revision AND p.run_generation=NEW.run_generation AND (p.predecessor_operation_id IS NULL OR EXISTS(SELECT 1 FROM import_journal predecessor JOIN import_history h ON h.operation_id=predecessor.operation_id WHERE predecessor.operation_id=p.predecessor_operation_id AND predecessor.phase='complete')) AND j.phase='preview' AND j.path_failure_code IS NULL AND j.error_code IS NULL AND o.source=c.source_path AND o.destination=c.destination AND o.mode=c.resolved_mode AND NEW.quality_id=c.quality_id AND NEW.revision_json=c.revision_json AND NEW.edition IS c.edition AND ((p.media_type='movies' AND o.media_type='movie' AND o.movie_id=c.movie_id) OR (p.media_type='tv' AND o.media_type='episode' AND o.episode_id=(SELECT min(m.episode_id) FROM downloaded_scan_child_episodes m WHERE m.child_id=c.id)))) THEN RAISE(ABORT,'invalid scan producer authority') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM rss_candidate_imports WHERE operation_id=NEW.operation_id) OR EXISTS(SELECT 1 FROM manual_import_commands WHERE operation_id=NEW.operation_id) THEN RAISE(ABORT,'operation already has a producer') END;
 -- Quality revisions are bounded nonnegative integers, distinct from protocol envelope version1.
 SELECT CASE WHEN json_type(NEW.revision_json,'$.version') IS NOT 'integer' OR json_extract(NEW.revision_json,'$.version') NOT BETWEEN 0 AND 2147483647 OR json_type(NEW.revision_json,'$.real') IS NOT 'integer' OR json_extract(NEW.revision_json,'$.real') NOT BETWEEN 0 AND 2147483647 OR json_type(NEW.revision_json,'$.is_repack') NOT IN ('true','false') OR json_type(NEW.revision_json,'$.is_repack') IS NULL OR (SELECT count(*) FROM json_each(NEW.revision_json))!=3 THEN RAISE(ABORT,'invalid scan quality revision') END;
 SELECT CASE WHEN (json_type(NEW.comparison_facts_json,'$.version') IS NOT 'integer' OR json_extract(NEW.comparison_facts_json,'$.version') IS NOT 1) THEN RAISE(ABORT,'invalid scan comparison facts') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM downloaded_scan_child_episodes m JOIN episodes e ON e.id=m.episode_id WHERE m.child_id=NEW.child_id AND e.episode_file_id IS NOT m.expected_old_episode_file_id) OR EXISTS(SELECT 1 FROM downloaded_scan_children c WHERE c.id=NEW.child_id AND c.movie_id IS NOT NULL AND c.expected_old_movie_file_id IS NOT (SELECT id FROM movie_files WHERE movie_id=c.movie_id)) THEN RAISE(ABORT,'scan old association changed') END;
END;
CREATE TRIGGER scan_import_facts_immutable BEFORE UPDATE ON scan_import_facts
BEGIN SELECT RAISE(ABORT,'scan producer facts are immutable'); END;
CREATE TRIGGER scan_replacement_admit BEFORE INSERT ON scan_import_replacements BEGIN
 SELECT CASE WHEN NEW.retirement_state!='pending' OR NEW.retirement_json IS NOT NULL OR NOT EXISTS(SELECT 1 FROM scan_import_facts f JOIN downloaded_scan_children c ON c.id=f.child_id JOIN downloaded_scan_commands p ON p.id=f.command_id JOIN import_journal j ON j.operation_id=f.operation_id WHERE f.operation_id=NEW.operation_id AND j.phase='preview' AND c.operation_id IS NULL AND p.status='running' AND p.cancel_requested=0 AND p.run_generation=f.run_generation AND p.media_type=NEW.media_type AND ((NEW.media_type='tv' AND EXISTS(SELECT 1 FROM downloaded_scan_child_episodes m JOIN episode_files e ON e.id=m.expected_old_episode_file_id WHERE m.child_id=c.id AND e.id=NEW.old_episode_file_id AND e.path=json_extract(NEW.old_file_json,'$.path'))) OR (NEW.media_type='movies' AND EXISTS(SELECT 1 FROM movie_files m WHERE m.id=NEW.old_movie_file_id AND m.id=c.expected_old_movie_file_id AND m.movie_id=c.movie_id AND m.path=json_extract(NEW.old_file_json,'$.path'))))) THEN RAISE(ABORT,'invalid scan replacement authority') END;
 SELECT CASE WHEN (json_type(NEW.old_file_json,'$.version') IS NOT 'integer' OR json_extract(NEW.old_file_json,'$.version') IS NOT 1) OR json_extract(NEW.old_file_json,'$.file_id') IS NOT NEW.old_file_id OR json_type(NEW.old_file_json,'$.path') IS NOT 'text' OR length(CAST(json_extract(NEW.old_file_json,'$.path') AS BLOB)) NOT BETWEEN 1 AND 4096 OR instr(json_extract(NEW.old_file_json,'$.path'),char(0))!=0 OR json_extract(NEW.old_file_json,'$.quarantine_name') IS NOT '.hrrdarr-replaced-'||NEW.operation_id||'-'||NEW.old_file_id THEN RAISE(ABORT,'invalid scan replacement archive') END;
 SELECT CASE WHEN (SELECT v.bytes+coalesce((SELECT sum(length(CAST(r.old_file_json AS BLOB))+4096) FROM scan_import_replacements r JOIN scan_import_facts f ON f.operation_id=r.operation_id WHERE f.command_id=v.id),0) FROM downloaded_scan_inventory_size v JOIN scan_import_facts f ON f.command_id=v.id WHERE f.operation_id=NEW.operation_id)+length(CAST(NEW.old_file_json AS BLOB))+4096>67108864 THEN RAISE(ABORT,'scan inventory byte capacity reached') END;
END;
CREATE TRIGGER scan_replacement_transition BEFORE UPDATE ON scan_import_replacements BEGIN
 SELECT CASE WHEN NEW.operation_id IS NOT OLD.operation_id OR NEW.media_type IS NOT OLD.media_type OR NEW.old_file_id IS NOT OLD.old_file_id OR NEW.old_episode_file_id IS NOT OLD.old_episode_file_id OR NEW.old_movie_file_id IS NOT OLD.old_movie_file_id OR NEW.old_file_json IS NOT OLD.old_file_json THEN RAISE(ABORT,'scan replacement facts are immutable') END;
 SELECT CASE WHEN OLD.retirement_state!='pending' OR NOT EXISTS(SELECT 1 FROM import_journal WHERE operation_id=OLD.operation_id AND phase='committed') THEN RAISE(ABORT,'scan retirement requires committed import') END;
 SELECT CASE WHEN OLD.retirement_json IS NOT NULL AND json_extract(NEW.retirement_json,'$.directory') IS NOT json_extract(OLD.retirement_json,'$.directory') THEN RAISE(ABORT,'scan retirement anchor is immutable') END;
 SELECT CASE WHEN NEW.retirement_json IS NOT NULL AND json_type(NEW.retirement_json,'$.directory') IS NOT 'object' THEN RAISE(ABORT,'scan retirement requires directory anchor') END;
 SELECT CASE WHEN NEW.retirement_state='shared_retained' AND (NEW.media_type!='tv' OR NOT EXISTS(SELECT 1 FROM episodes WHERE episode_file_id=NEW.old_episode_file_id) OR NOT EXISTS(SELECT 1 FROM episode_files WHERE id=NEW.old_episode_file_id AND path=json_extract(NEW.old_file_json,'$.path'))) THEN RAISE(ABORT,'scan shared retention requires unchanged live original') END;
 SELECT CASE WHEN NEW.retirement_state='quarantined' AND ((NEW.media_type='tv' AND (EXISTS(SELECT 1 FROM episodes WHERE episode_file_id=NEW.old_episode_file_id) OR NOT EXISTS(SELECT 1 FROM episode_files WHERE id=NEW.old_episode_file_id AND path!=json_extract(NEW.old_file_json,'$.path'))))) THEN RAISE(ABORT,'scan quarantine requires detached original') END;
END;
CREATE TRIGGER scan_link_requires_replacements BEFORE UPDATE OF operation_id ON downloaded_scan_children
WHEN OLD.operation_id IS NULL AND NEW.operation_id IS NOT NULL
BEGIN
 SELECT CASE WHEN EXISTS(SELECT 1 FROM downloaded_scan_child_episodes m WHERE m.child_id=NEW.id AND m.expected_old_episode_file_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM scan_import_replacements r WHERE r.operation_id=NEW.operation_id AND r.old_episode_file_id=m.expected_old_episode_file_id)) OR (NEW.expected_old_movie_file_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM scan_import_replacements r WHERE r.operation_id=NEW.operation_id AND r.old_movie_file_id=NEW.expected_old_movie_file_id)) THEN RAISE(ABORT,'scan link requires every replacement archive') END;
END;
CREATE TRIGGER scan_history_member_admit BEFORE INSERT ON import_history_episodes BEGIN
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM import_history h JOIN scan_import_facts f ON f.operation_id=h.operation_id JOIN downloaded_scan_child_episodes m ON m.child_id=f.child_id JOIN episodes e ON e.id=m.episode_id JOIN episode_files ef ON ef.id=e.episode_file_id WHERE h.operation_id=NEW.operation_id AND h.media_type='episode' AND m.episode_id=NEW.episode_id AND e.series_id=m.series_id AND ef.series_id=m.series_id AND ef.id=NEW.episode_file_id AND h.episode_file_id=ef.id AND ef.path=h.destination) THEN RAISE(ABORT,'invalid scan history membership') END;
END;
CREATE TRIGGER scan_history_member_immutable BEFORE UPDATE ON import_history_episodes
BEGIN SELECT RAISE(ABORT,'scan history membership is immutable'); END;
CREATE TRIGGER scan_journal_commit BEFORE UPDATE OF phase ON import_journal
WHEN EXISTS(SELECT 1 FROM scan_import_facts WHERE operation_id=NEW.operation_id)
BEGIN
 SELECT CASE WHEN OLD.phase='preview' AND NEW.phase!='preview' AND NOT EXISTS(SELECT 1 FROM scan_import_facts f JOIN downloaded_scan_children c ON c.id=f.child_id JOIN downloaded_scan_commands p ON p.id=f.command_id WHERE f.operation_id=NEW.operation_id AND c.operation_id=f.operation_id AND c.disposition='importing' AND p.status='running' AND p.inventory_revision=f.inventory_revision AND p.cancel_requested=0) THEN RAISE(ABORT,'scan effect requires linked active child') END;
 SELECT CASE WHEN OLD.phase='preview' AND NEW.phase!='preview' AND NEW.path_failure_code IS NULL THEN RAISE(ABORT,'scan effect requires unresolved path evidence') END;
 SELECT CASE WHEN NEW.phase IN ('committed','complete') AND EXISTS(SELECT 1 FROM scan_import_facts f JOIN downloaded_scan_child_episodes m ON m.child_id=f.child_id WHERE f.operation_id=NEW.operation_id AND NOT EXISTS(SELECT 1 FROM import_history_episodes h WHERE h.operation_id=NEW.operation_id AND h.episode_id=m.episode_id)) THEN RAISE(ABORT,'scan commit requires every member history') END;
 SELECT CASE WHEN NEW.phase='complete' AND EXISTS(SELECT 1 FROM scan_import_replacements r WHERE r.operation_id=NEW.operation_id AND r.retirement_state='pending') THEN RAISE(ABORT,'scan retirement must complete') END;
END;

-- Expected membership is sealed with the manifest; credits are write-once historical proof.
CREATE TRIGGER scan_coverage_admit BEFORE INSERT ON downloaded_scan_coverage BEGIN
 SELECT CASE WHEN NEW.credited_operation_id IS NOT NULL OR NOT EXISTS(SELECT 1 FROM downloaded_scan_commands p JOIN rss_candidates r ON r.id=p.candidate_id WHERE p.id=NEW.command_id AND p.status='running' AND p.inventory_revision=0 AND p.cancel_requested=0 AND ((p.media_type='movies' AND NEW.movie_id=r.movie_id AND NEW.episode_id IS NULL) OR (p.media_type='tv' AND NEW.movie_id IS NULL AND EXISTS(SELECT 1 FROM rss_candidate_episodes e WHERE e.candidate_id=r.id AND e.episode_id=NEW.episode_id)))) THEN RAISE(ABORT,'invalid scan expected coverage') END;
 SELECT CASE WHEN (SELECT count(*) FROM downloaded_scan_coverage WHERE command_id=NEW.command_id)>=20000 THEN RAISE(ABORT,'scan expected coverage capacity reached') END;
END;
CREATE TRIGGER scan_coverage_credit BEFORE UPDATE ON downloaded_scan_coverage BEGIN
 SELECT CASE WHEN NEW.command_id IS NOT OLD.command_id OR NEW.episode_id IS NOT OLD.episode_id OR NEW.movie_id IS NOT OLD.movie_id OR OLD.credited_operation_id IS NOT NULL OR NEW.credited_operation_id IS NULL THEN RAISE(ABORT,'scan coverage is write-once') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM downloaded_scan_commands p JOIN import_journal j ON j.operation_id=NEW.credited_operation_id JOIN import_history h ON h.operation_id=j.operation_id WHERE p.id=NEW.command_id AND p.status='running' AND p.inventory_revision=1 AND j.phase='complete' AND ((h.operation_id=p.predecessor_operation_id) OR EXISTS(SELECT 1 FROM downloaded_scan_children c WHERE c.command_id=p.id AND c.operation_id=h.operation_id AND c.disposition='imported') OR EXISTS(SELECT 1 FROM downloaded_scan_coverage v WHERE v.command_id=p.supersedes_command_id AND v.credited_operation_id=h.operation_id AND v.episode_id IS NEW.episode_id AND v.movie_id IS NEW.movie_id AND v.credited_file_id=NEW.credited_file_id)) AND ((NEW.movie_id IS NOT NULL AND h.media_type='movie' AND h.movie_id=NEW.movie_id AND h.movie_file_id=NEW.credited_file_id AND EXISTS(SELECT 1 FROM movie_files mf WHERE mf.id=NEW.credited_file_id AND mf.movie_id=NEW.movie_id AND mf.path=h.destination)) OR (NEW.episode_id IS NOT NULL AND h.media_type='episode' AND ((h.episode_id=NEW.episode_id AND h.episode_file_id=NEW.credited_file_id) OR EXISTS(SELECT 1 FROM import_history_episodes hm WHERE hm.operation_id=h.operation_id AND hm.episode_id=NEW.episode_id AND hm.episode_file_id=NEW.credited_file_id)) AND EXISTS(SELECT 1 FROM episodes e JOIN episode_files ef ON ef.id=e.episode_file_id AND ef.series_id=e.series_id WHERE e.id=NEW.episode_id AND ef.id=NEW.credited_file_id AND ef.path=h.destination)))) THEN RAISE(ABORT,'scan coverage requires current typed historical proof') END;
END;

-- Scan handback bypasses only the singleton transition proof, never its receipt lifetime fence.
CREATE TRIGGER scan_processing_insert BEFORE INSERT ON download_processing
WHEN NEW.scan_command_id IS NOT NULL OR EXISTS(SELECT 1 FROM downloaded_scan_commands WHERE candidate_id=NEW.candidate_id)
BEGIN SELECT RAISE(ABORT,'retained scan proof owns processing admission'); END;
CREATE TRIGGER scan_processing_legacy_guard BEFORE UPDATE ON download_processing
WHEN NEW.scan_command_id IS NULL AND (OLD.scan_command_id IS NOT NULL OR EXISTS(SELECT 1 FROM downloaded_scan_commands p WHERE p.candidate_id=NEW.candidate_id AND NOT EXISTS(SELECT 1 FROM rss_candidate_imports i WHERE i.candidate_id=NEW.candidate_id AND i.operation_id=p.predecessor_operation_id)))
BEGIN SELECT RAISE(ABORT,'retained scan proof requires coverage-aware recovery'); END;
CREATE TRIGGER scan_processing_transition BEFORE UPDATE ON download_processing
WHEN NEW.scan_command_id IS NOT NULL
BEGIN
 SELECT CASE WHEN NEW.candidate_id IS NOT OLD.candidate_id OR NEW.policy_revision IS NOT OLD.policy_revision OR NEW.preflight_attempts IS NOT OLD.preflight_attempts OR NEW.total_preflight_attempts IS NOT OLD.total_preflight_attempts OR NEW.next_attempt_at IS NOT OLD.next_attempt_at OR NEW.created_at IS NOT OLD.created_at OR NEW.resume_requested!=0 OR NEW.updated_at<OLD.updated_at OR OLD.status NOT IN ('queued','blocked','cancelled') OR EXISTS(SELECT 1 FROM rss_candidate_imports WHERE candidate_id=NEW.candidate_id) THEN RAISE(ABORT,'scan handback changed processing lifetime') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM downloaded_scan_commands p WHERE p.id=NEW.scan_command_id AND p.candidate_id=NEW.candidate_id AND p.status IN ('succeeded','failed','cancelled') AND p.inventory_revision=1 AND p.cleanup_state!='pending' AND ((p.coverage_state='complete' AND NEW.status='imported' AND NEW.error_code IS NULL AND NEW.reasons_json='[]') OR (p.coverage_state='incomplete' AND NEW.status='blocked' AND NEW.error_code='import_failed' AND EXISTS(SELECT 1 FROM json_each(NEW.reasons_json) WHERE value='downloaded_scan_incomplete'))) AND (json_type(p.processing_fence_json,'$.version') IS 'integer' AND json_extract(p.processing_fence_json,'$.version') IS 1) AND json_extract(p.processing_fence_json,'$.policy_revision') IS OLD.policy_revision AND json_extract(p.processing_fence_json,'$.status') IS OLD.status AND json_extract(p.processing_fence_json,'$.resume_requested') IS OLD.resume_requested AND json_extract(p.processing_fence_json,'$.preflight_attempts') IS OLD.preflight_attempts AND json_extract(p.processing_fence_json,'$.total_preflight_attempts') IS OLD.total_preflight_attempts AND json_extract(p.processing_fence_json,'$.next_attempt_at') IS OLD.next_attempt_at AND json_extract(p.processing_fence_json,'$.created_at') IS OLD.created_at AND json_extract(p.processing_fence_json,'$.updated_at') IS OLD.updated_at AND json_extract(p.processing_fence_json,'$.error_code') IS OLD.error_code AND json_extract(p.processing_fence_json,'$.reasons_json') IS OLD.reasons_json AND json_extract(p.processing_fence_json,'$.scan_command_id') IS OLD.scan_command_id) THEN RAISE(ABORT,'scan handback requires exact captured receipt and settlement') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM json_each(NEW.reasons_json) WHERE type!='text' OR length(value)>128 OR value GLOB '*[^a-z0-9_]*') OR (SELECT count(*) FROM json_each(NEW.reasons_json))!=(SELECT count(DISTINCT value) FROM json_each(NEW.reasons_json)) THEN RAISE(ABORT,'invalid scan handback reasons') END;
 SELECT CASE WHEN NEW.status='blocked' AND (SELECT value FROM json_each(OLD.reasons_json) WHERE key=0 AND value LIKE 'hc021_%') IS NOT (SELECT value FROM json_each(NEW.reasons_json) WHERE key=0 AND value LIKE 'hc021_%') THEN RAISE(ABORT,'partial scan cannot clear retained path evidence') END;
END;

CREATE TRIGGER scan_blocks_rss_link BEFORE INSERT ON rss_candidate_imports
WHEN EXISTS(SELECT 1 FROM scan_import_facts WHERE operation_id=NEW.operation_id) OR EXISTS(SELECT 1 FROM downloaded_scan_commands WHERE candidate_id=NEW.candidate_id)
BEGIN SELECT RAISE(ABORT,'scan proof excludes singleton import producer'); END;
CREATE TRIGGER scan_blocks_manual_admit BEFORE INSERT ON manual_import_commands
WHEN EXISTS(SELECT 1 FROM scan_import_facts WHERE operation_id=NEW.operation_id)
 OR EXISTS(SELECT 1 FROM operations o JOIN downloaded_scan_reserved_children c ON c.source_path=o.source OR c.destination=o.destination OR (o.media_type='movie' AND c.movie_id=o.movie_id) OR (o.media_type='episode' AND EXISTS(SELECT 1 FROM downloaded_scan_child_episodes m WHERE m.child_id=c.id AND m.episode_id=o.episode_id)) WHERE o.id=NEW.operation_id)
BEGIN SELECT RAISE(ABORT,'scan reservation excludes manual import'); END;
CREATE TRIGGER scan_blocks_rescan BEFORE INSERT ON rescan_commands
WHEN EXISTS(SELECT 1 FROM downloaded_scan_reserved_children c WHERE (NEW.media_type='tv' AND c.series_id=NEW.series_id) OR (NEW.media_type='movies' AND c.movie_id=NEW.movie_id))
BEGIN SELECT RAISE(ABORT,'scan reservation excludes rescan'); END;
CREATE TRIGGER scan_blocks_rss_target BEFORE UPDATE OF status ON rss_candidates
WHEN NEW.status='prepared' AND EXISTS(SELECT 1 FROM downloaded_scan_reserved_children c WHERE (NEW.media_type='movies' AND c.movie_id=NEW.movie_id) OR (NEW.media_type='tv' AND EXISTS(SELECT 1 FROM downloaded_scan_child_episodes m JOIN rss_candidate_episodes e ON e.episode_id=m.episode_id WHERE m.child_id=c.id AND e.candidate_id=NEW.id)))
BEGIN SELECT RAISE(ABORT,'scan reservation excludes RSS target'); END;
CREATE TRIGGER scan_blocks_journal_insert BEFORE INSERT ON import_journal
WHEN NEW.phase!='preview' AND EXISTS(SELECT 1 FROM operations o JOIN downloaded_scan_reserved_children c ON c.source_path=o.source OR c.destination=o.destination OR (o.media_type='movie' AND c.movie_id=o.movie_id) OR (o.media_type='episode' AND EXISTS(SELECT 1 FROM downloaded_scan_child_episodes m WHERE m.child_id=c.id AND m.episode_id=o.episode_id)) WHERE o.id=NEW.operation_id)
BEGIN SELECT RAISE(ABORT,'scan reservation excludes direct transfer'); END;
CREATE TRIGGER scan_blocks_journal_arm BEFORE UPDATE OF phase ON import_journal
WHEN OLD.phase='preview' AND NEW.phase!='preview' AND EXISTS(SELECT 1 FROM operations o JOIN downloaded_scan_reserved_children c ON c.source_path=o.source OR c.destination=o.destination OR (o.media_type='movie' AND c.movie_id=o.movie_id) OR (o.media_type='episode' AND EXISTS(SELECT 1 FROM downloaded_scan_child_episodes m WHERE m.child_id=c.id AND m.episode_id=o.episode_id)) JOIN downloaded_scan_commands p ON p.id=c.command_id WHERE o.id=NEW.operation_id AND c.operation_id IS NOT NEW.operation_id AND p.predecessor_operation_id IS NOT NEW.operation_id)
BEGIN SELECT RAISE(ABORT,'scan reservation excludes direct transfer'); END;
CREATE TRIGGER scan_child_existing_transfer BEFORE INSERT ON downloaded_scan_children
WHEN NEW.selected=1 AND EXISTS(SELECT 1 FROM operations o JOIN import_journal j ON j.operation_id=o.id JOIN downloaded_scan_commands p ON p.id=NEW.command_id WHERE j.phase NOT IN ('preview','complete') AND o.id IS NOT p.predecessor_operation_id AND (o.source=NEW.source_path OR o.destination=NEW.destination OR (NEW.movie_id IS NOT NULL AND o.movie_id=NEW.movie_id)))
BEGIN SELECT RAISE(ABORT,'scan conflicts with existing transfer'); END;
CREATE TRIGGER scan_member_existing_transfer BEFORE INSERT ON downloaded_scan_child_episodes
WHEN EXISTS(SELECT 1 FROM operations o JOIN import_journal j ON j.operation_id=o.id JOIN downloaded_scan_commands p ON p.id=NEW.command_id WHERE o.episode_id=NEW.episode_id AND o.media_type='episode' AND j.phase NOT IN ('preview','complete') AND o.id IS NOT p.predecessor_operation_id)
BEGIN SELECT RAISE(ABORT,'scan member conflicts with existing transfer'); END;
CREATE TRIGGER scan_series_identity BEFORE UPDATE OF path ON series
WHEN NEW.path IS NOT OLD.path AND EXISTS(SELECT 1 FROM downloaded_scan_reserved_children WHERE series_id=OLD.id)
BEGIN SELECT RAISE(ABORT,'scan reserves series identity'); END;
CREATE TRIGGER scan_movie_identity BEFORE UPDATE OF path ON movies
WHEN NEW.path IS NOT OLD.path AND EXISTS(SELECT 1 FROM downloaded_scan_reserved_children WHERE movie_id=OLD.id)
BEGIN SELECT RAISE(ABORT,'scan reserves movie identity'); END;
-- Frozen old associations may change only in the corresponding published commit transaction.
CREATE TRIGGER scan_episode_association BEFORE UPDATE OF episode_file_id ON episodes
WHEN NEW.episode_file_id IS NOT OLD.episode_file_id AND EXISTS(SELECT 1 FROM downloaded_scan_child_episodes m JOIN downloaded_scan_reserved_children c ON c.id=m.child_id LEFT JOIN import_journal j ON j.operation_id=c.operation_id WHERE m.episode_id=OLD.id AND (j.phase IS NOT 'published' OR (NEW.episode_file_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM episode_files f WHERE f.id=NEW.episode_file_id AND f.series_id=NEW.series_id AND f.path=c.destination))))
BEGIN SELECT RAISE(ABORT,'scan association requires published child'); END;
CREATE TRIGGER scan_episode_owner BEFORE UPDATE OF series_id,season,number ON episodes
WHEN (NEW.series_id IS NOT OLD.series_id OR NEW.season IS NOT OLD.season OR NEW.number IS NOT OLD.number) AND EXISTS(SELECT 1 FROM downloaded_scan_child_episodes m JOIN downloaded_scan_reserved_children c ON c.id=m.child_id WHERE m.episode_id=OLD.id)
BEGIN SELECT RAISE(ABORT,'scan reserves episode identity'); END;
CREATE TRIGGER scan_old_episode_attach BEFORE INSERT ON episodes
WHEN EXISTS(SELECT 1 FROM scan_import_replacements r JOIN import_journal j ON j.operation_id=r.operation_id WHERE r.old_episode_file_id=NEW.episode_file_id AND (j.phase!='complete' OR r.retirement_state='quarantined'))
BEGIN SELECT RAISE(ABORT,'scan replacement cannot gain associations'); END;
CREATE TRIGGER scan_old_episode_reattach BEFORE UPDATE OF episode_file_id ON episodes
WHEN NEW.episode_file_id IS NOT OLD.episode_file_id AND EXISTS(SELECT 1 FROM scan_import_replacements r JOIN import_journal j ON j.operation_id=r.operation_id WHERE r.old_episode_file_id=NEW.episode_file_id AND (j.phase!='complete' OR r.retirement_state='quarantined'))
BEGIN SELECT RAISE(ABORT,'scan replacement cannot gain associations'); END;

-- The original singleton exchange predicate is preserved; scans add complete-member proof.
DROP TRIGGER same_path_admit;
CREATE TRIGGER same_path_admit BEFORE INSERT ON same_path_replacements BEGIN
 SELECT CASE WHEN NEW.state!='exchange_intent' OR (NOT EXISTS(
 SELECT 1 FROM import_journal j JOIN operations o ON o.id=j.operation_id JOIN rss_candidate_imports i ON i.operation_id=j.operation_id
 WHERE j.operation_id=NEW.operation_id AND j.version=1 AND j.phase='staged'
 AND json_type(j.stage_json,'$.sha256')='text' AND length(json_extract(j.stage_json,'$.sha256'))=64 AND json_extract(j.stage_json,'$.sha256') NOT GLOB '*[^0-9a-f]*'
 AND o.destination=json_extract(j.plan_json,'$.destination') AND o.destination=json_extract(i.old_file_json,'$.path')
 AND i.retirement_state='pending' AND i.retirement_json IS NULL
 AND ((o.media_type='episode' AND EXISTS(SELECT 1 FROM episodes e JOIN episode_files f ON f.id=e.episode_file_id WHERE e.id=o.episode_id AND f.id=i.old_episode_file_id AND f.path=o.destination) AND (SELECT count(*) FROM episodes WHERE episode_file_id=i.old_episode_file_id)=1)
 OR (o.media_type='movie' AND EXISTS(SELECT 1 FROM movie_files f WHERE f.movie_id=o.movie_id AND f.id=i.old_movie_file_id AND f.path=o.destination))))
 AND NOT EXISTS(SELECT 1 FROM scan_import_facts f JOIN scan_import_replacements r ON r.operation_id=f.operation_id JOIN downloaded_scan_children c ON c.id=f.child_id JOIN operations o ON o.id=f.operation_id JOIN import_journal j ON j.operation_id=o.id WHERE f.operation_id=NEW.operation_id AND c.operation_id=f.operation_id AND j.phase='staged' AND json_type(j.stage_json,'$.sha256')='text' AND length(json_extract(j.stage_json,'$.sha256'))=64 AND json_extract(j.stage_json,'$.sha256') NOT GLOB '*[^0-9a-f]*' AND o.destination=json_extract(j.plan_json,'$.destination') AND o.destination=json_extract(r.old_file_json,'$.path') AND r.retirement_state='pending' AND r.retirement_json IS NULL AND (SELECT count(*) FROM scan_import_replacements other WHERE other.operation_id=f.operation_id AND json_extract(other.old_file_json,'$.path')=o.destination)=1 AND ((o.media_type='episode' AND EXISTS(SELECT 1 FROM episode_files ef WHERE ef.id=r.old_episode_file_id AND ef.path=o.destination AND ef.series_id=c.series_id) AND NOT EXISTS(SELECT 1 FROM episodes e WHERE e.episode_file_id=r.old_episode_file_id AND NOT EXISTS(SELECT 1 FROM downloaded_scan_child_episodes m WHERE m.child_id=c.id AND m.episode_id=e.id))) OR (o.media_type='movie' AND EXISTS(SELECT 1 FROM movie_files mf WHERE mf.id=r.old_movie_file_id AND mf.movie_id=o.movie_id AND mf.path=o.destination)))))
 THEN RAISE(ABORT,'same path exchange requires staged owned unshared original') END;
END;

DROP TRIGGER rss_movie_target_owned;
CREATE TRIGGER rss_movie_target_owned BEFORE UPDATE OF status ON rss_candidates
WHEN NEW.status='prepared' AND EXISTS(SELECT 1 FROM rss_candidates owner WHERE owner.id!=NEW.id AND owner.movie_id=NEW.movie_id AND owner.status IN ('prepared','submitting','reconciling','observed','needs_attention') AND NOT EXISTS(SELECT 1 FROM rss_candidate_imports i JOIN import_journal j ON j.operation_id=i.operation_id JOIN import_history h ON h.operation_id=j.operation_id WHERE i.candidate_id=owner.id AND j.phase='complete') AND NOT EXISTS(SELECT 1 FROM downloaded_scan_complete_coverage v WHERE v.candidate_id=owner.id))
BEGIN SELECT RAISE(ABORT,'movie target already owned'); END;

DROP TRIGGER rss_episode_target_owned;
CREATE TRIGGER rss_episode_target_owned BEFORE UPDATE OF status ON rss_candidates
WHEN NEW.status='prepared' AND EXISTS(SELECT 1 FROM rss_candidate_episodes incoming JOIN rss_candidate_episodes existing ON existing.episode_id=incoming.episode_id JOIN rss_candidates owner ON owner.id=existing.candidate_id WHERE incoming.candidate_id=NEW.id AND owner.id!=NEW.id AND owner.status IN ('prepared','submitting','reconciling','observed','needs_attention') AND NOT EXISTS(SELECT 1 FROM rss_candidate_imports i JOIN import_journal j ON j.operation_id=i.operation_id JOIN import_history h ON h.operation_id=j.operation_id WHERE i.candidate_id=owner.id AND j.phase='complete') AND NOT EXISTS(SELECT 1 FROM downloaded_scan_complete_coverage v WHERE v.candidate_id=owner.id))
BEGIN SELECT RAISE(ABORT,'episode target already owned'); END;
CREATE TRIGGER scan_same_path_commit BEFORE UPDATE OF phase ON import_journal
WHEN NEW.phase IN ('committed','complete') AND EXISTS(SELECT 1 FROM scan_import_replacements r WHERE r.operation_id=NEW.operation_id AND json_extract(r.old_file_json,'$.path')=json_extract(NEW.plan_json,'$.destination')) AND NOT EXISTS(SELECT 1 FROM same_path_replacements WHERE operation_id=NEW.operation_id AND state='installed')
BEGIN SELECT RAISE(ABORT,'scan same path commit requires installed exchange'); END;
CREATE TRIGGER scan_old_episode_path BEFORE UPDATE OF path ON episode_files
WHEN NEW.path IS NOT OLD.path AND EXISTS(SELECT 1 FROM scan_import_replacements r JOIN import_journal j ON j.operation_id=r.operation_id WHERE r.old_episode_file_id=OLD.id AND (j.phase!='complete' OR r.retirement_state='quarantined'))
 AND NOT EXISTS(SELECT 1 FROM scan_import_replacements r JOIN import_journal j ON j.operation_id=r.operation_id WHERE r.old_episode_file_id=OLD.id AND r.retirement_state='pending' AND NOT EXISTS(SELECT 1 FROM episodes WHERE episode_file_id=OLD.id) AND NEW.path=rtrim(json_extract(r.old_file_json,'$.path'),replace(json_extract(r.old_file_json,'$.path'),'/',''))||json_extract(r.old_file_json,'$.quarantine_name')||'/original' AND (j.phase='committed' OR (j.phase='published' AND EXISTS(SELECT 1 FROM same_path_replacements x WHERE x.operation_id=j.operation_id AND x.state='installed'))))
BEGIN SELECT RAISE(ABORT,'scan old path requires retirement checkpoint'); END;
CREATE TRIGGER scan_old_movie_path BEFORE UPDATE OF path ON movie_files
WHEN NEW.path IS NOT OLD.path AND EXISTS(SELECT 1 FROM scan_import_replacements r JOIN import_journal j ON j.operation_id=r.operation_id WHERE r.old_movie_file_id=OLD.id AND j.phase!='complete')
 AND NOT EXISTS(SELECT 1 FROM scan_import_replacements r JOIN import_journal j ON j.operation_id=r.operation_id JOIN operations o ON o.id=j.operation_id WHERE r.old_movie_file_id=OLD.id AND r.retirement_state='pending' AND j.phase='published' AND NEW.path=o.destination)
BEGIN SELECT RAISE(ABORT,'scan movie replacement requires published child'); END;
CREATE TRIGGER scan_library_settings_update BEFORE UPDATE ON library_settings
WHEN EXISTS(SELECT 1 FROM downloaded_scan_reserved_children c WHERE c.series_id=OLD.series_id OR c.movie_id=OLD.movie_id OR c.series_id=NEW.series_id OR c.movie_id=NEW.movie_id)
BEGIN SELECT RAISE(ABORT,'scan reserves library settings'); END;
CREATE TRIGGER scan_library_settings_insert BEFORE INSERT ON library_settings
WHEN EXISTS(SELECT 1 FROM downloaded_scan_reserved_children c WHERE c.series_id=NEW.series_id OR c.movie_id=NEW.movie_id)
BEGIN SELECT RAISE(ABORT,'scan reserves library settings'); END;
CREATE TRIGGER scan_library_settings_delete BEFORE DELETE ON library_settings
WHEN EXISTS(SELECT 1 FROM downloaded_scan_reserved_children c WHERE c.series_id=OLD.series_id OR c.movie_id=OLD.movie_id)
BEGIN SELECT RAISE(ABORT,'scan reserves library settings'); END;
CREATE TRIGGER scan_replacement_path_unique BEFORE INSERT ON scan_import_replacements
WHEN ((SELECT count(*) FROM episode_files WHERE path=json_extract(NEW.old_file_json,'$.path'))+(SELECT count(*) FROM movie_files WHERE path=json_extract(NEW.old_file_json,'$.path')))!=1
 OR EXISTS(SELECT 1 FROM rss_candidate_imports i JOIN import_journal j ON j.operation_id=i.operation_id WHERE j.phase!='complete' AND json_extract(i.old_file_json,'$.path')=json_extract(NEW.old_file_json,'$.path'))
 OR EXISTS(SELECT 1 FROM scan_import_replacements r JOIN import_journal j ON j.operation_id=r.operation_id WHERE j.phase!='complete' AND r.operation_id!=NEW.operation_id AND json_extract(r.old_file_json,'$.path')=json_extract(NEW.old_file_json,'$.path'))
BEGIN SELECT RAISE(ABORT,'scan original path has conflicting ownership'); END;
CREATE TRIGGER scan_blocks_rss_old_path BEFORE INSERT ON rss_candidate_imports
WHEN EXISTS(SELECT 1 FROM scan_import_replacements r JOIN import_journal j ON j.operation_id=r.operation_id WHERE j.phase!='complete' AND json_extract(r.old_file_json,'$.path')=json_extract(NEW.old_file_json,'$.path'))
BEGIN SELECT RAISE(ABORT,'scan owns original path'); END;
CREATE TRIGGER scan_old_path_episode_insert BEFORE INSERT ON episode_files
WHEN EXISTS(SELECT 1 FROM scan_import_replacements r JOIN import_journal j ON j.operation_id=r.operation_id WHERE j.phase!='complete' AND json_extract(r.old_file_json,'$.path')=NEW.path AND NOT EXISTS(SELECT 1 FROM same_path_replacements x JOIN scan_import_facts f ON f.operation_id=x.operation_id JOIN downloaded_scan_children c ON c.id=f.child_id JOIN episode_files old ON old.id=r.old_episode_file_id WHERE x.operation_id=r.operation_id AND x.state='installed' AND j.phase='published' AND c.series_id=NEW.series_id AND NOT EXISTS(SELECT 1 FROM episodes WHERE episode_file_id=old.id) AND old.path=rtrim(json_extract(r.old_file_json,'$.path'),replace(json_extract(r.old_file_json,'$.path'),'/',''))||json_extract(r.old_file_json,'$.quarantine_name')||'/original'))
BEGIN SELECT RAISE(ABORT,'scan owns original path'); END;
CREATE TRIGGER scan_old_path_episode_update BEFORE UPDATE OF path ON episode_files
WHEN NEW.path IS NOT OLD.path AND EXISTS(SELECT 1 FROM scan_import_replacements r JOIN import_journal j ON j.operation_id=r.operation_id WHERE j.phase!='complete' AND json_extract(r.old_file_json,'$.path')=NEW.path)
BEGIN SELECT RAISE(ABORT,'scan owns original path'); END;
CREATE TRIGGER scan_old_path_movie_insert BEFORE INSERT ON movie_files
WHEN EXISTS(SELECT 1 FROM scan_import_replacements r JOIN import_journal j ON j.operation_id=r.operation_id WHERE j.phase!='complete' AND json_extract(r.old_file_json,'$.path')=NEW.path)
BEGIN SELECT RAISE(ABORT,'scan owns original path'); END;
CREATE TRIGGER scan_old_path_movie_update BEFORE UPDATE OF path ON movie_files
WHEN NEW.path IS NOT OLD.path AND EXISTS(SELECT 1 FROM scan_import_replacements r JOIN import_journal j ON j.operation_id=r.operation_id WHERE j.phase!='complete' AND json_extract(r.old_file_json,'$.path')=NEW.path)
BEGIN SELECT RAISE(ABORT,'scan owns original path'); END;

CREATE UNIQUE INDEX downloaded_scan_child_destination_unique ON downloaded_scan_children(command_id,destination) WHERE selected=1;
CREATE TRIGGER scan_child_byte_capacity BEFORE INSERT ON downloaded_scan_children
WHEN (SELECT bytes FROM downloaded_scan_inventory_size WHERE id=NEW.command_id)+length(CAST(NEW.relative_path AS BLOB))+length(CAST(NEW.source_path AS BLOB))+coalesce(length(CAST(NEW.source_identity_json AS BLOB)),0)+length(CAST(NEW.decision_json AS BLOB))+length(CAST(NEW.reasons_json AS BLOB))+coalesce(length(CAST(NEW.destination AS BLOB)),0)+coalesce(length(CAST(NEW.revision_json AS BLOB)),0)+coalesce(length(CAST(NEW.edition AS BLOB)),0)+128>67108864
BEGIN SELECT RAISE(ABORT,'scan inventory byte capacity reached'); END;

-- Bound lineage before accepting any ancestor-derived authority; overflow is not a partial proof.
CREATE TRIGGER scan_successor_lineage BEFORE INSERT ON downloaded_scan_commands
WHEN NEW.supersedes_command_id IS NOT NULL
BEGIN
 SELECT CASE WHEN EXISTS(WITH RECURSIVE ancestry(id,previous,depth) AS (SELECT id,supersedes_command_id,1 FROM downloaded_scan_commands WHERE id=NEW.supersedes_command_id UNION ALL SELECT a.id,a.supersedes_command_id,x.depth+1 FROM downloaded_scan_commands a JOIN ancestry x ON a.id=x.previous WHERE x.depth<1024) SELECT 1 FROM ancestry WHERE depth=1024 AND previous IS NOT NULL) THEN RAISE(ABORT,'scan lineage capacity reached') END;
END;
CREATE TRIGGER scan_child_tracked_owner BEFORE INSERT ON downloaded_scan_children
WHEN NEW.selected=1 AND EXISTS(SELECT 1 FROM downloaded_scan_commands p JOIN rss_candidates r ON r.id=p.candidate_id WHERE p.id=NEW.command_id AND ((p.media_type='tv' AND NEW.series_id IS NOT r.series_id) OR (p.media_type='movies' AND NEW.movie_id IS NOT r.movie_id)))
BEGIN SELECT RAISE(ABORT,'scan child differs from tracked owner'); END;
CREATE TRIGGER scan_capture_processing_fence BEFORE UPDATE OF candidate_id ON downloaded_scan_commands
WHEN NEW.candidate_id IS NOT OLD.candidate_id
BEGIN
 SELECT CASE WHEN (json_type(NEW.processing_fence_json,'$.version') IS NOT 'integer' OR json_extract(NEW.processing_fence_json,'$.version') IS NOT 1) OR NOT ((json_type(NEW.processing_fence_json,'$.absent') IS 'true' AND (SELECT count(*) FROM json_each(NEW.processing_fence_json))=2 AND NOT EXISTS(SELECT 1 FROM download_processing d WHERE d.candidate_id=NEW.candidate_id)) OR ((SELECT count(*) FROM json_each(NEW.processing_fence_json))=12 AND coalesce(json_type(NEW.processing_fence_json,'$.error_code') IN ('null','text'),0) AND coalesce(json_type(NEW.processing_fence_json,'$.scan_command_id') IN ('null','text'),0) AND EXISTS(SELECT 1 FROM download_processing d WHERE d.candidate_id=NEW.candidate_id AND json_extract(NEW.processing_fence_json,'$.policy_revision') IS d.policy_revision AND json_extract(NEW.processing_fence_json,'$.status') IS d.status AND json_extract(NEW.processing_fence_json,'$.resume_requested') IS d.resume_requested AND json_extract(NEW.processing_fence_json,'$.preflight_attempts') IS d.preflight_attempts AND json_extract(NEW.processing_fence_json,'$.total_preflight_attempts') IS d.total_preflight_attempts AND json_extract(NEW.processing_fence_json,'$.next_attempt_at') IS d.next_attempt_at AND json_extract(NEW.processing_fence_json,'$.created_at') IS d.created_at AND json_extract(NEW.processing_fence_json,'$.updated_at') IS d.updated_at AND json_extract(NEW.processing_fence_json,'$.error_code') IS d.error_code AND json_extract(NEW.processing_fence_json,'$.reasons_json') IS d.reasons_json AND json_extract(NEW.processing_fence_json,'$.scan_command_id') IS d.scan_command_id))) THEN RAISE(ABORT,'scan requires exact processing fence') END;
END;
-- Waiting may retain readonly observations, but selected old associations belong to the
-- predecessor until its original journal/history have completed under original authority.
CREATE TRIGGER scan_child_waits_for_predecessor BEFORE INSERT ON downloaded_scan_children
WHEN NEW.selected=1 AND EXISTS(SELECT 1 FROM downloaded_scan_commands p WHERE p.id=NEW.command_id AND p.predecessor_operation_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM import_journal j JOIN import_history h ON h.operation_id=j.operation_id WHERE j.operation_id=p.predecessor_operation_id AND j.phase='complete'))
BEGIN SELECT RAISE(ABORT,'scan predecessor must settle before selecting children'); END;
-- inventory_bytes is the immutable publication fact; this indexed view measures current
-- retained bytes, including mutable reasons and the same reserved retirement budget.
CREATE TRIGGER scan_child_reason_byte_capacity BEFORE UPDATE OF reasons_json ON downloaded_scan_children
WHEN (SELECT v.bytes+coalesce((SELECT sum(length(CAST(r.old_file_json AS BLOB))+4096) FROM scan_import_replacements r JOIN scan_import_facts f ON f.operation_id=r.operation_id WHERE f.command_id=v.id),0) FROM downloaded_scan_inventory_size v WHERE v.id=NEW.command_id)+length(CAST(NEW.reasons_json AS BLOB))-length(CAST(OLD.reasons_json AS BLOB))>67108864
BEGIN SELECT RAISE(ABORT,'scan inventory byte capacity reached'); END;
