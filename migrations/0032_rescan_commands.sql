-- Durable per-target library rescans (adopt-in-place file reconciliation) share the existing command pool.
CREATE TABLE rescan_commands (
 id TEXT PRIMARY KEY NOT NULL CHECK(length(id)=36 AND substr(id,9,1)='-' AND substr(id,14,1)='-' AND substr(id,19,1)='-' AND substr(id,24,1)='-' AND length(replace(id,'-',''))=32 AND replace(id,'-','') NOT GLOB '*[^0-9a-f]*'),
 media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
 series_id INTEGER CHECK(series_id IS NULL OR (typeof(series_id)='integer' AND series_id BETWEEN 1 AND 9007199254740991)),
 movie_id INTEGER CHECK(movie_id IS NULL OR (typeof(movie_id)='integer' AND movie_id BETWEEN 1 AND 9007199254740991)),
 priority INTEGER NOT NULL DEFAULT 0 CHECK(typeof(priority)='integer' AND priority IN (0,1)),
 status TEXT NOT NULL DEFAULT 'queued' CHECK(status IN ('queued','running','retry_wait','succeeded','skipped','failed','cancelled')),
 attempts INTEGER NOT NULL DEFAULT 0 CHECK(typeof(attempts)='integer' AND attempts BETWEEN 0 AND 3),
 next_attempt_at INTEGER NOT NULL CHECK(typeof(next_attempt_at)='integer' AND next_attempt_at BETWEEN 0 AND 9007199254740991),
 created_at INTEGER NOT NULL CHECK(typeof(created_at)='integer' AND created_at BETWEEN 0 AND 9007199254740991),
 started_at INTEGER CHECK(started_at IS NULL OR (typeof(started_at)='integer' AND started_at BETWEEN 0 AND 9007199254740991)),
 completed_at INTEGER CHECK(completed_at IS NULL OR (typeof(completed_at)='integer' AND completed_at BETWEEN 0 AND 9007199254740991)),
 error_code TEXT CHECK(error_code IS NULL OR error_code IN ('interrupted','storage_error','target_changed')),
 skip_reason TEXT CHECK(skip_reason IS NULL OR skip_reason IN ('root_missing','root_empty')),
 files_adopted INTEGER CHECK(files_adopted IS NULL OR (typeof(files_adopted)='integer' AND files_adopted BETWEEN 0 AND 9007199254740991)),
 files_removed INTEGER CHECK(files_removed IS NULL OR (typeof(files_removed)='integer' AND files_removed BETWEEN 0 AND 9007199254740991)),
 CHECK((media_type='tv' AND series_id IS NOT NULL AND movie_id IS NULL) OR (media_type='movies' AND movie_id IS NOT NULL AND series_id IS NULL)),
 CHECK((status IN ('queued','running','retry_wait') AND completed_at IS NULL) OR (status IN ('succeeded','skipped','failed','cancelled') AND completed_at IS NOT NULL)),
 CHECK(status!='queued' OR (attempts=0 AND started_at IS NULL)),
 CHECK(status NOT IN ('running','succeeded','skipped','retry_wait') OR (attempts>0 AND started_at IS NOT NULL)),
 CHECK(status!='retry_wait' OR attempts<3),
 CHECK((status IN ('failed','retry_wait') AND error_code IS NOT NULL) OR (status NOT IN ('failed','retry_wait') AND error_code IS NULL)),
 CHECK((status='skipped' AND skip_reason IS NOT NULL) OR (status!='skipped' AND skip_reason IS NULL)),
 CHECK((status='succeeded' AND files_adopted IS NOT NULL AND files_removed IS NOT NULL) OR (status!='succeeded' AND files_adopted IS NULL AND files_removed IS NULL))
);
CREATE UNIQUE INDEX rescan_active_series ON rescan_commands(series_id) WHERE status IN ('queued','running','retry_wait');
CREATE UNIQUE INDEX rescan_active_movie ON rescan_commands(movie_id) WHERE status IN ('queued','running','retry_wait');
CREATE INDEX rescan_ready ON rescan_commands(status,next_attempt_at,priority,created_at,id);
CREATE INDEX rescan_history ON rescan_commands(created_at,id);
CREATE TRIGGER rescan_admit BEFORE INSERT ON rescan_commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands)+(SELECT count(*) FROM metadata_refresh_commands)+(SELECT count(*) FROM blocklist_clear_commands)+(SELECT count(*) FROM rss_commands)+(SELECT count(*) FROM search_commands)+(SELECT count(*) FROM manual_import_commands)+(SELECT count(*) FROM quality_reset_commands)+(SELECT count(*) FROM rescan_commands)>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT ((NEW.media_type='tv' AND EXISTS(SELECT 1 FROM series WHERE id=NEW.series_id)) OR (NEW.media_type='movies' AND EXISTS(SELECT 1 FROM movies WHERE id=NEW.movie_id))) THEN RAISE(ABORT,'invalid rescan target') END;
 -- 'preview' does not touch the filesystem yet and may never be executed, so it alone must not
 -- block a rescan forever; 'complete' is done. The window that matters is real transfer work.
 SELECT CASE WHEN EXISTS(SELECT 1 FROM import_journal j JOIN operations o ON o.id=j.operation_id WHERE j.phase NOT IN ('preview','complete') AND ((NEW.media_type='tv' AND o.media_type='episode' AND EXISTS(SELECT 1 FROM episodes e WHERE e.id=o.episode_id AND e.series_id=NEW.series_id)) OR (NEW.media_type='movies' AND o.media_type='movie' AND o.movie_id=NEW.movie_id))) THEN RAISE(ABORT,'rescan target has an in-flight import') END;
 -- 'queued' has not started preflight/transfer work; 'blocked'/'cancelled' are paused, not active.
 SELECT CASE WHEN EXISTS(SELECT 1 FROM download_processing dp JOIN rss_candidates r ON r.id=dp.candidate_id WHERE dp.status IN ('checking','importing') AND ((NEW.media_type='tv' AND r.media_type='tv' AND r.series_id=NEW.series_id) OR (NEW.media_type='movies' AND r.media_type='movies' AND r.movie_id=NEW.movie_id))) THEN RAISE(ABORT,'rescan target has in-flight download processing') END;
END;
CREATE TRIGGER rescan_transition BEFORE UPDATE ON rescan_commands
BEGIN
 SELECT CASE WHEN NEW.id IS NOT OLD.id OR NEW.media_type IS NOT OLD.media_type OR NEW.series_id IS NOT OLD.series_id OR NEW.movie_id IS NOT OLD.movie_id OR NEW.created_at IS NOT OLD.created_at OR NEW.priority IS NOT OLD.priority THEN RAISE(ABORT,'command identity is immutable') END;
 SELECT CASE WHEN NOT ((OLD.status IN ('queued','retry_wait') AND NEW.status IN ('running','cancelled','failed')) OR (OLD.status='running' AND NEW.status IN ('running','retry_wait','succeeded','skipped','failed','cancelled'))) THEN RAISE(ABORT,'invalid command transition') END;
 SELECT CASE WHEN NEW.attempts != OLD.attempts + CASE WHEN NEW.status='running' AND OLD.status IN ('queued','retry_wait') THEN 1 ELSE 0 END THEN RAISE(ABORT,'invalid command attempt counter') END;
 SELECT CASE WHEN NEW.status IN ('running','succeeded','skipped') AND NOT ((NEW.media_type='tv' AND EXISTS(SELECT 1 FROM series WHERE id=NEW.series_id)) OR (NEW.media_type='movies' AND EXISTS(SELECT 1 FROM movies WHERE id=NEW.movie_id))) THEN RAISE(ABORT,'stale rescan target') END;
 -- Re-verified defense in depth: the reverse guards on import_journal/download_processing below
 -- already prevent a conflicting import/download from starting while this row is queued/running,
 -- so this should be unreachable in practice, but a worker must never start scanning unprotected.
 SELECT CASE WHEN NEW.status='running' AND OLD.status IN ('queued','retry_wait') AND EXISTS(SELECT 1 FROM import_journal j JOIN operations o ON o.id=j.operation_id WHERE j.phase NOT IN ('preview','complete') AND ((NEW.media_type='tv' AND o.media_type='episode' AND EXISTS(SELECT 1 FROM episodes e WHERE e.id=o.episode_id AND e.series_id=NEW.series_id)) OR (NEW.media_type='movies' AND o.media_type='movie' AND o.movie_id=NEW.movie_id))) THEN RAISE(ABORT,'rescan target has an in-flight import') END;
 SELECT CASE WHEN NEW.status='running' AND OLD.status IN ('queued','retry_wait') AND EXISTS(SELECT 1 FROM download_processing dp JOIN rss_candidates r ON r.id=dp.candidate_id WHERE dp.status IN ('checking','importing') AND ((NEW.media_type='tv' AND r.media_type='tv' AND r.series_id=NEW.series_id) OR (NEW.media_type='movies' AND r.media_type='movies' AND r.movie_id=NEW.movie_id))) THEN RAISE(ABORT,'rescan target has in-flight download processing') END;
END;
CREATE TRIGGER rescan_delete_terminal BEFORE DELETE ON rescan_commands WHEN OLD.status IN ('queued','running','retry_wait')
BEGIN SELECT RAISE(ABORT,'active commands cannot be deleted'); END;
-- The reverse direction: an import/download must not start real filesystem work while a rescan is
-- active for the same target, mirroring the forward checks in rescan_admit/rescan_transition above.
CREATE TRIGGER rescan_blocks_import_insert BEFORE INSERT ON import_journal
WHEN NEW.phase!='preview' AND EXISTS(SELECT 1 FROM rescan_commands r JOIN operations o ON o.id=NEW.operation_id WHERE r.status IN ('queued','running','retry_wait') AND ((o.media_type='episode' AND r.media_type='tv' AND EXISTS(SELECT 1 FROM episodes e WHERE e.id=o.episode_id AND e.series_id=r.series_id)) OR (o.media_type='movie' AND r.media_type='movies' AND r.movie_id=o.movie_id)))
BEGIN SELECT RAISE(ABORT,'import target has an active rescan'); END;
CREATE TRIGGER rescan_blocks_import_update BEFORE UPDATE OF phase ON import_journal
WHEN OLD.phase='preview' AND NEW.phase!='preview' AND EXISTS(SELECT 1 FROM rescan_commands r JOIN operations o ON o.id=NEW.operation_id WHERE r.status IN ('queued','running','retry_wait') AND ((o.media_type='episode' AND r.media_type='tv' AND EXISTS(SELECT 1 FROM episodes e WHERE e.id=o.episode_id AND e.series_id=r.series_id)) OR (o.media_type='movie' AND r.media_type='movies' AND r.movie_id=o.movie_id)))
BEGIN SELECT RAISE(ABORT,'import target has an active rescan'); END;
CREATE TRIGGER rescan_blocks_processing BEFORE UPDATE OF status ON download_processing
WHEN NEW.status='checking' AND OLD.status!='checking' AND EXISTS(SELECT 1 FROM rescan_commands r JOIN rss_candidates c ON c.id=NEW.candidate_id WHERE r.status IN ('queued','running','retry_wait') AND ((c.media_type='tv' AND r.media_type='tv' AND r.series_id=c.series_id) OR (c.media_type='movies' AND r.media_type='movies' AND r.movie_id=c.movie_id)))
BEGIN SELECT RAISE(ABORT,'download processing target has an active rescan'); END;
-- Admission changes only; existing sibling command rows, triggers and referenced observations remain intact.
DROP TRIGGER commands_admit;
CREATE TRIGGER commands_admit BEFORE INSERT ON commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands)+(SELECT count(*) FROM metadata_refresh_commands)+(SELECT count(*) FROM blocklist_clear_commands)+(SELECT count(*) FROM rss_commands)+(SELECT count(*) FROM search_commands)+(SELECT count(*) FROM manual_import_commands)+(SELECT count(*) FROM quality_reset_commands)+(SELECT count(*) FROM rescan_commands)>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.provider_id AND p.revision=NEW.provider_revision AND p.enabled=1 AND p.implementation='qbittorrent' AND s.media_type=NEW.media_type) THEN RAISE(ABORT,'invalid refresh provider scope or revision') END;
END;
DROP TRIGGER metadata_refresh_admit;
CREATE TRIGGER metadata_refresh_admit BEFORE INSERT ON metadata_refresh_commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands)+(SELECT count(*) FROM metadata_refresh_commands)+(SELECT count(*) FROM blocklist_clear_commands)+(SELECT count(*) FROM rss_commands)+(SELECT count(*) FROM search_commands)+(SELECT count(*) FROM manual_import_commands)+(SELECT count(*) FROM quality_reset_commands)+(SELECT count(*) FROM rescan_commands)>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM series s WHERE s.id=NEW.series_id AND s.tvdb_id=NEW.external_id) AND NOT EXISTS(SELECT 1 FROM movies m JOIN movie_metadata d ON d.id=m.metadata_id WHERE m.id=NEW.movie_id AND m.metadata_id=NEW.metadata_id AND d.tmdb_id=NEW.external_id) THEN RAISE(ABORT,'invalid metadata refresh target') END;
END;
DROP TRIGGER blocklist_clear_admit;
CREATE TRIGGER blocklist_clear_admit BEFORE INSERT ON blocklist_clear_commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands)+(SELECT count(*) FROM metadata_refresh_commands)+(SELECT count(*) FROM blocklist_clear_commands)+(SELECT count(*) FROM rss_commands)+(SELECT count(*) FROM search_commands)+(SELECT count(*) FROM manual_import_commands)+(SELECT count(*) FROM quality_reset_commands)+(SELECT count(*) FROM rescan_commands)>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
END;
DROP TRIGGER rss_commands_admit;
CREATE TRIGGER rss_commands_admit BEFORE INSERT ON rss_commands BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands)+(SELECT count(*) FROM metadata_refresh_commands)+(SELECT count(*) FROM blocklist_clear_commands)+(SELECT count(*) FROM rss_commands)+(SELECT count(*) FROM search_commands)+(SELECT count(*) FROM manual_import_commands)+(SELECT count(*) FROM quality_reset_commands)+(SELECT count(*) FROM rescan_commands)>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 OR NEW.fetch_complete!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT (EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.indexer_id AND p.revision=NEW.indexer_revision AND p.implementation IN ('torznab','newznab') AND s.media_type=NEW.media_type AND p.enabled=1) AND EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.client_id AND p.revision=NEW.client_revision AND p.implementation='qbittorrent' AND s.media_type=NEW.media_type AND p.enabled=1)) THEN RAISE(ABORT,'invalid RSS provider scope or revision') END;
END;
DROP TRIGGER search_commands_admit;
CREATE TRIGGER search_commands_admit BEFORE INSERT ON search_commands BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands)+(SELECT count(*) FROM metadata_refresh_commands)+(SELECT count(*) FROM blocklist_clear_commands)+(SELECT count(*) FROM rss_commands)+(SELECT count(*) FROM search_commands)+(SELECT count(*) FROM manual_import_commands)+(SELECT count(*) FROM quality_reset_commands)+(SELECT count(*) FROM rescan_commands)>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 OR NEW.fetch_complete!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT (EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.indexer_id AND p.revision=NEW.indexer_revision AND p.implementation IN ('torznab','newznab') AND s.media_type=NEW.media_type AND p.enabled=1) AND EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.client_id AND p.revision=NEW.client_revision AND p.implementation='qbittorrent' AND s.media_type=NEW.media_type AND p.enabled=1)) THEN RAISE(ABORT,'invalid RSS provider scope or revision') END;
 SELECT CASE WHEN NOT ((NEW.media_type='tv' AND EXISTS(SELECT 1 FROM episodes e WHERE e.id=NEW.requested_episode_id AND json_extract(NEW.captured_target_json,'$.media_type')='tv' AND json_extract(NEW.captured_target_json,'$.episode_id')=e.id AND json_extract(NEW.captured_target_json,'$.series_id')=e.series_id)) OR (NEW.media_type='movies' AND EXISTS(SELECT 1 FROM movies m WHERE m.id=NEW.requested_movie_id AND json_extract(NEW.captured_target_json,'$.media_type')='movies' AND json_extract(NEW.captured_target_json,'$.movie_id')=m.id AND json_extract(NEW.captured_target_json,'$.metadata_id')=m.metadata_id))) THEN RAISE(ABORT,'invalid search target') END;
END;
DROP TRIGGER manual_import_commands_admit;
CREATE TRIGGER manual_import_commands_admit BEFORE INSERT ON manual_import_commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands)+(SELECT count(*) FROM metadata_refresh_commands)+(SELECT count(*) FROM blocklist_clear_commands)+(SELECT count(*) FROM rss_commands)+(SELECT count(*) FROM search_commands)+(SELECT count(*) FROM manual_import_commands)+(SELECT count(*) FROM quality_reset_commands)+(SELECT count(*) FROM rescan_commands)>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM import_journal WHERE operation_id=NEW.operation_id) THEN RAISE(ABORT,'operation has no import journal') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM rss_candidate_imports WHERE operation_id=NEW.operation_id) THEN RAISE(ABORT,'operation is owned by automated download import') END;
END;
DROP TRIGGER quality_reset_admit;
CREATE TRIGGER quality_reset_admit BEFORE INSERT ON quality_reset_commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands)+(SELECT count(*) FROM metadata_refresh_commands)+(SELECT count(*) FROM blocklist_clear_commands)+(SELECT count(*) FROM rss_commands)+(SELECT count(*) FROM search_commands)+(SELECT count(*) FROM manual_import_commands)+(SELECT count(*) FROM quality_reset_commands)+(SELECT count(*) FROM rescan_commands)>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
END;
