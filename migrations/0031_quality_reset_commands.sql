-- Explicit whole-domain quality-definition reset shares the existing locally owned command worker.
CREATE TABLE quality_reset_commands (
 id TEXT PRIMARY KEY NOT NULL CHECK(length(id)=36 AND substr(id,9,1)='-' AND substr(id,14,1)='-' AND substr(id,19,1)='-' AND substr(id,24,1)='-' AND length(replace(id,'-',''))=32 AND replace(id,'-','') NOT GLOB '*[^0-9a-f]*'),
 media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
 reset_titles INTEGER NOT NULL CHECK(reset_titles IN (0,1)),
 priority INTEGER NOT NULL DEFAULT 0 CHECK(typeof(priority)='integer' AND priority IN (0,1)),
 status TEXT NOT NULL DEFAULT 'queued' CHECK(status IN ('queued','running','retry_wait','succeeded','failed','cancelled')),
 attempts INTEGER NOT NULL DEFAULT 0 CHECK(typeof(attempts)='integer' AND attempts BETWEEN 0 AND 3),
 next_attempt_at INTEGER NOT NULL CHECK(typeof(next_attempt_at)='integer' AND next_attempt_at BETWEEN 0 AND 9007199254740991),
 created_at INTEGER NOT NULL CHECK(typeof(created_at)='integer' AND created_at BETWEEN 0 AND 9007199254740991),
 started_at INTEGER CHECK(started_at IS NULL OR (typeof(started_at)='integer' AND started_at BETWEEN 0 AND 9007199254740991)),
 completed_at INTEGER CHECK(completed_at IS NULL OR (typeof(completed_at)='integer' AND completed_at BETWEEN 0 AND 9007199254740991)),
 error_code TEXT CHECK(error_code IS NULL OR error_code IN ('interrupted','storage_error')),
 definitions_reset INTEGER CHECK(definitions_reset IS NULL OR (typeof(definitions_reset)='integer' AND definitions_reset BETWEEN 0 AND 9007199254740991)),
 CHECK((status IN ('queued','running','retry_wait') AND completed_at IS NULL) OR (status IN ('succeeded','failed','cancelled') AND completed_at IS NOT NULL)),
 CHECK(status!='queued' OR (attempts=0 AND started_at IS NULL)),
 CHECK(status NOT IN ('running','succeeded','retry_wait') OR (attempts>0 AND started_at IS NOT NULL)),
 CHECK(status!='retry_wait' OR attempts<3),
 CHECK((status IN ('failed','retry_wait') AND error_code IS NOT NULL) OR (status NOT IN ('failed','retry_wait') AND error_code IS NULL)),
 CHECK((status='succeeded' AND definitions_reset IS NOT NULL) OR (status!='succeeded' AND definitions_reset IS NULL))
);
CREATE UNIQUE INDEX quality_reset_active ON quality_reset_commands(media_type) WHERE status IN ('queued','running','retry_wait');
CREATE INDEX quality_reset_ready ON quality_reset_commands(status,next_attempt_at,priority,created_at,id);
CREATE INDEX quality_reset_history ON quality_reset_commands(created_at,id);
CREATE TRIGGER quality_reset_admit BEFORE INSERT ON quality_reset_commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands)+(SELECT count(*) FROM metadata_refresh_commands)+(SELECT count(*) FROM blocklist_clear_commands)+(SELECT count(*) FROM rss_commands)+(SELECT count(*) FROM search_commands)+(SELECT count(*) FROM manual_import_commands)+(SELECT count(*) FROM quality_reset_commands)>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
END;
CREATE TRIGGER quality_reset_transition BEFORE UPDATE ON quality_reset_commands
BEGIN
 SELECT CASE WHEN NEW.id IS NOT OLD.id OR NEW.media_type IS NOT OLD.media_type OR NEW.reset_titles IS NOT OLD.reset_titles OR NEW.created_at IS NOT OLD.created_at OR NEW.priority IS NOT OLD.priority THEN RAISE(ABORT,'command identity is immutable') END;
 SELECT CASE WHEN NOT ((OLD.status IN ('queued','retry_wait') AND NEW.status IN ('running','cancelled','failed')) OR (OLD.status='running' AND NEW.status IN ('running','retry_wait','succeeded','failed','cancelled'))) THEN RAISE(ABORT,'invalid command transition') END;
 SELECT CASE WHEN NEW.attempts != OLD.attempts + CASE WHEN NEW.status='running' AND OLD.status IN ('queued','retry_wait') THEN 1 ELSE 0 END THEN RAISE(ABORT,'invalid command attempt counter') END;
END;
CREATE TRIGGER quality_reset_delete_terminal BEFORE DELETE ON quality_reset_commands WHEN OLD.status IN ('queued','running','retry_wait')
BEGIN SELECT RAISE(ABORT,'active commands cannot be deleted'); END;
-- Admission changes only; existing sibling command rows, triggers and referenced observations remain intact.
DROP TRIGGER commands_admit;
CREATE TRIGGER commands_admit BEFORE INSERT ON commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands)+(SELECT count(*) FROM metadata_refresh_commands)+(SELECT count(*) FROM blocklist_clear_commands)+(SELECT count(*) FROM rss_commands)+(SELECT count(*) FROM search_commands)+(SELECT count(*) FROM manual_import_commands)+(SELECT count(*) FROM quality_reset_commands)>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.provider_id AND p.revision=NEW.provider_revision AND p.enabled=1 AND p.implementation='qbittorrent' AND s.media_type=NEW.media_type) THEN RAISE(ABORT,'invalid refresh provider scope or revision') END;
END;
DROP TRIGGER metadata_refresh_admit;
CREATE TRIGGER metadata_refresh_admit BEFORE INSERT ON metadata_refresh_commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands)+(SELECT count(*) FROM metadata_refresh_commands)+(SELECT count(*) FROM blocklist_clear_commands)+(SELECT count(*) FROM rss_commands)+(SELECT count(*) FROM search_commands)+(SELECT count(*) FROM manual_import_commands)+(SELECT count(*) FROM quality_reset_commands)>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM series s WHERE s.id=NEW.series_id AND s.tvdb_id=NEW.external_id) AND NOT EXISTS(SELECT 1 FROM movies m JOIN movie_metadata d ON d.id=m.metadata_id WHERE m.id=NEW.movie_id AND m.metadata_id=NEW.metadata_id AND d.tmdb_id=NEW.external_id) THEN RAISE(ABORT,'invalid metadata refresh target') END;
END;
DROP TRIGGER blocklist_clear_admit;
CREATE TRIGGER blocklist_clear_admit BEFORE INSERT ON blocklist_clear_commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands)+(SELECT count(*) FROM metadata_refresh_commands)+(SELECT count(*) FROM blocklist_clear_commands)+(SELECT count(*) FROM rss_commands)+(SELECT count(*) FROM search_commands)+(SELECT count(*) FROM manual_import_commands)+(SELECT count(*) FROM quality_reset_commands)>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
END;
DROP TRIGGER rss_commands_admit;
CREATE TRIGGER rss_commands_admit BEFORE INSERT ON rss_commands BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands)+(SELECT count(*) FROM metadata_refresh_commands)+(SELECT count(*) FROM blocklist_clear_commands)+(SELECT count(*) FROM rss_commands)+(SELECT count(*) FROM search_commands)+(SELECT count(*) FROM manual_import_commands)+(SELECT count(*) FROM quality_reset_commands)>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 OR NEW.fetch_complete!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT (EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.indexer_id AND p.revision=NEW.indexer_revision AND p.implementation IN ('torznab','newznab') AND s.media_type=NEW.media_type AND p.enabled=1) AND EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.client_id AND p.revision=NEW.client_revision AND p.implementation='qbittorrent' AND s.media_type=NEW.media_type AND p.enabled=1)) THEN RAISE(ABORT,'invalid RSS provider scope or revision') END;
END;
DROP TRIGGER search_commands_admit;
CREATE TRIGGER search_commands_admit BEFORE INSERT ON search_commands BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands)+(SELECT count(*) FROM metadata_refresh_commands)+(SELECT count(*) FROM blocklist_clear_commands)+(SELECT count(*) FROM rss_commands)+(SELECT count(*) FROM search_commands)+(SELECT count(*) FROM manual_import_commands)+(SELECT count(*) FROM quality_reset_commands)>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 OR NEW.fetch_complete!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT (EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.indexer_id AND p.revision=NEW.indexer_revision AND p.implementation IN ('torznab','newznab') AND s.media_type=NEW.media_type AND p.enabled=1) AND EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.client_id AND p.revision=NEW.client_revision AND p.implementation='qbittorrent' AND s.media_type=NEW.media_type AND p.enabled=1)) THEN RAISE(ABORT,'invalid RSS provider scope or revision') END;
 SELECT CASE WHEN NOT ((NEW.media_type='tv' AND EXISTS(SELECT 1 FROM episodes e WHERE e.id=NEW.requested_episode_id AND json_extract(NEW.captured_target_json,'$.media_type')='tv' AND json_extract(NEW.captured_target_json,'$.episode_id')=e.id AND json_extract(NEW.captured_target_json,'$.series_id')=e.series_id)) OR (NEW.media_type='movies' AND EXISTS(SELECT 1 FROM movies m WHERE m.id=NEW.requested_movie_id AND json_extract(NEW.captured_target_json,'$.media_type')='movies' AND json_extract(NEW.captured_target_json,'$.movie_id')=m.id AND json_extract(NEW.captured_target_json,'$.metadata_id')=m.metadata_id))) THEN RAISE(ABORT,'invalid search target') END;
END;
DROP TRIGGER manual_import_commands_admit;
CREATE TRIGGER manual_import_commands_admit BEFORE INSERT ON manual_import_commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands)+(SELECT count(*) FROM metadata_refresh_commands)+(SELECT count(*) FROM blocklist_clear_commands)+(SELECT count(*) FROM rss_commands)+(SELECT count(*) FROM search_commands)+(SELECT count(*) FROM manual_import_commands)+(SELECT count(*) FROM quality_reset_commands)>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM import_journal WHERE operation_id=NEW.operation_id) THEN RAISE(ABORT,'operation has no import journal') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM rss_candidate_imports WHERE operation_id=NEW.operation_id) THEN RAISE(ABORT,'operation is owned by automated download import') END;
END;
