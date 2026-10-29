-- File/folder rename commands (cmd.011/cmd.012), their crash-recovery journal and the durable
-- rename event outbox. One command table, domain-typed targets; shares the 1024-row command pool.
CREATE TABLE rename_commands (
 id TEXT PRIMARY KEY NOT NULL CHECK(typeof(id)='text' AND length(id)=36 AND substr(id,9,1)='-' AND substr(id,14,1)='-' AND substr(id,19,1)='-' AND substr(id,24,1)='-' AND length(replace(id,'-',''))=32 AND replace(id,'-','') NOT GLOB '*[^0-9a-f]*'),
 name TEXT NOT NULL CHECK(name IN ('rename_files','rename_series','rename_movie','rename_series_folder','rename_movie_folder')),
 media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
 expected_naming_revision INTEGER CHECK(expected_naming_revision IS NULL OR (typeof(expected_naming_revision)='integer' AND expected_naming_revision BETWEEN 1 AND 9007199254740991)),
 priority INTEGER NOT NULL DEFAULT 0 CHECK(typeof(priority)='integer' AND priority IN (0,1)),
 status TEXT NOT NULL DEFAULT 'queued' CHECK(status IN ('queued','running','retry_wait','succeeded','failed','cancelled')),
 attempts INTEGER NOT NULL DEFAULT 0 CHECK(typeof(attempts)='integer' AND attempts BETWEEN 0 AND 3),
 next_attempt_at INTEGER NOT NULL CHECK(typeof(next_attempt_at)='integer' AND next_attempt_at BETWEEN 0 AND 9007199254740991),
 created_at INTEGER NOT NULL CHECK(typeof(created_at)='integer' AND created_at BETWEEN 0 AND 9007199254740991),
 started_at INTEGER CHECK(started_at IS NULL OR (typeof(started_at)='integer' AND started_at BETWEEN 0 AND 9007199254740991)),
 completed_at INTEGER CHECK(completed_at IS NULL OR (typeof(completed_at)='integer' AND completed_at BETWEEN 0 AND 9007199254740991)),
 error_code TEXT CHECK(error_code IS NULL OR error_code IN ('interrupted','storage_error','import_busy','naming_revision_changed','naming_configuration_invalid','target_missing','items_failed')),
 cancel_requested INTEGER NOT NULL DEFAULT 0 CHECK(cancel_requested IN (0,1)),
 items_renamed INTEGER NOT NULL DEFAULT 0 CHECK(items_renamed BETWEEN 0 AND 9007199254740991),
 items_unchanged INTEGER NOT NULL DEFAULT 0 CHECK(items_unchanged BETWEEN 0 AND 9007199254740991),
 items_failed INTEGER NOT NULL DEFAULT 0 CHECK(items_failed BETWEEN 0 AND 9007199254740991),
 bounded INTEGER NOT NULL DEFAULT 0 CHECK(bounded IN (0,1)),
 CHECK((name IN ('rename_files','rename_series','rename_series_folder') AND media_type='tv') OR (name IN ('rename_files','rename_movie','rename_movie_folder') AND media_type='movies')),
 CHECK((status IN ('queued','running','retry_wait') AND completed_at IS NULL) OR (status IN ('succeeded','failed','cancelled') AND completed_at IS NOT NULL)),
 CHECK(status!='queued' OR (attempts=0 AND started_at IS NULL)),
 CHECK(status NOT IN ('running','succeeded','retry_wait') OR (attempts>0 AND started_at IS NOT NULL)),
 CHECK(status!='retry_wait' OR attempts<3),
 CHECK((status IN ('failed','retry_wait') AND error_code IS NOT NULL) OR (status NOT IN ('failed','retry_wait') AND error_code IS NULL))
);
CREATE INDEX rename_ready ON rename_commands(status,next_attempt_at,priority,created_at,id);
CREATE INDEX rename_history ON rename_commands(created_at,id);
CREATE TABLE rename_command_targets (
 command_id TEXT NOT NULL REFERENCES rename_commands(id) ON DELETE CASCADE,
 ordinal INTEGER NOT NULL CHECK(ordinal BETWEEN 0 AND 399),
 series_id INTEGER CHECK(series_id IS NULL OR (typeof(series_id)='integer' AND series_id BETWEEN 1 AND 9007199254740991)),
 movie_id INTEGER CHECK(movie_id IS NULL OR (typeof(movie_id)='integer' AND movie_id BETWEEN 1 AND 9007199254740991)),
 file_id INTEGER CHECK(file_id IS NULL OR (typeof(file_id)='integer' AND file_id BETWEEN 1 AND 9007199254740991)),
 PRIMARY KEY(command_id,ordinal),
 CHECK((series_id IS NULL)!=(movie_id IS NULL))
);
CREATE TABLE rename_items (
 id INTEGER PRIMARY KEY,
 command_id TEXT NOT NULL REFERENCES rename_commands(id) ON DELETE CASCADE,
 kind TEXT NOT NULL CHECK(kind IN ('file','folder')),
 series_id INTEGER CHECK(series_id IS NULL OR series_id BETWEEN 1 AND 9007199254740991),
 movie_id INTEGER CHECK(movie_id IS NULL OR movie_id BETWEEN 1 AND 9007199254740991),
 file_id INTEGER,
 old_path TEXT NOT NULL CHECK(length(CAST(old_path AS BLOB)) BETWEEN 2 AND 4096),
 new_path TEXT NOT NULL CHECK(length(CAST(new_path AS BLOB)) BETWEEN 2 AND 4096),
 identity_json TEXT NOT NULL CHECK(json_valid(identity_json)),
 sidecars_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(sidecars_json) AND json_type(sidecars_json)='array' AND json_array_length(sidecars_json)<=64),
 state TEXT NOT NULL DEFAULT 'planned' CHECK(state IN ('planned','committed','failed','rolled_back')),
 error_code TEXT CHECK(error_code IS NULL OR length(error_code) BETWEEN 1 AND 64),
 CHECK((series_id IS NULL)!=(movie_id IS NULL)),
 CHECK(kind='folder' OR file_id IS NOT NULL)
);
CREATE INDEX rename_items_command ON rename_items(command_id,id);
CREATE TABLE rename_events (
 id TEXT PRIMARY KEY NOT NULL CHECK(typeof(id)='text' AND length(id)=36),
 event_type TEXT NOT NULL DEFAULT 'rename' CHECK(event_type='rename'),
 kind TEXT NOT NULL CHECK(kind IN ('file','folder')),
 media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
 series_id INTEGER CHECK(series_id IS NULL OR series_id BETWEEN 1 AND 9007199254740991),
 movie_id INTEGER CHECK(movie_id IS NULL OR movie_id BETWEEN 1 AND 9007199254740991),
 file_id INTEGER,
 command_id TEXT NOT NULL,
 old_path TEXT NOT NULL,
 new_path TEXT NOT NULL,
 sidecar_count INTEGER NOT NULL DEFAULT 0 CHECK(sidecar_count BETWEEN 0 AND 64),
 occurred_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
 dispatched_at TEXT,
 CHECK((media_type='tv' AND series_id IS NOT NULL AND movie_id IS NULL) OR (media_type='movies' AND movie_id IS NOT NULL AND series_id IS NULL))
);
CREATE INDEX rename_events_pending ON rename_events(occurred_at,id) WHERE dispatched_at IS NULL;
CREATE INDEX rename_events_order ON rename_events(occurred_at DESC,id DESC);
CREATE TRIGGER rename_events_immutable BEFORE UPDATE ON rename_events
WHEN NEW.id IS NOT OLD.id OR NEW.kind IS NOT OLD.kind OR NEW.media_type IS NOT OLD.media_type OR NEW.series_id IS NOT OLD.series_id OR NEW.movie_id IS NOT OLD.movie_id OR NEW.file_id IS NOT OLD.file_id OR NEW.command_id IS NOT OLD.command_id OR NEW.old_path IS NOT OLD.old_path OR NEW.new_path IS NOT OLD.new_path OR NEW.occurred_at IS NOT OLD.occurred_at OR (OLD.dispatched_at IS NOT NULL AND NEW.dispatched_at IS NOT OLD.dispatched_at)
BEGIN SELECT RAISE(ABORT,'rename events are immutable except first dispatch'); END;
CREATE TRIGGER rename_events_retained BEFORE DELETE ON rename_events
BEGIN SELECT RAISE(ABORT,'rename events must be retained'); END;
CREATE TRIGGER rename_admit BEFORE INSERT ON rename_commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM recycle_bin_cleanup_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rename_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 OR NEW.cancel_requested!=0 OR NEW.items_renamed!=0 OR NEW.items_unchanged!=0 OR NEW.items_failed!=0 OR NEW.bounded!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
END;
CREATE TRIGGER rename_transition BEFORE UPDATE ON rename_commands
BEGIN
 SELECT CASE WHEN NEW.id IS NOT OLD.id OR NEW.name IS NOT OLD.name OR NEW.media_type IS NOT OLD.media_type OR NEW.created_at IS NOT OLD.created_at OR NEW.priority IS NOT OLD.priority OR NEW.expected_naming_revision IS NOT OLD.expected_naming_revision THEN RAISE(ABORT,'command identity is immutable') END;
 -- Any state may still receive journal-recovery counters (a recovered item settles after the
 -- command was interrupted, retried or finished), and nothing else.
 SELECT CASE WHEN NOT ((OLD.status IN ('queued','retry_wait') AND NEW.status IN ('running','cancelled','failed')) OR (OLD.status='running' AND NEW.status IN ('running','retry_wait','succeeded','failed','cancelled')) OR (NEW.status=OLD.status AND NEW.completed_at IS OLD.completed_at AND NEW.error_code IS OLD.error_code AND NEW.started_at IS OLD.started_at AND NEW.next_attempt_at=OLD.next_attempt_at AND NEW.cancel_requested=OLD.cancel_requested AND NEW.bounded=OLD.bounded)) THEN RAISE(ABORT,'invalid command transition') END;
 SELECT CASE WHEN NEW.attempts != OLD.attempts + CASE WHEN NEW.status='running' AND OLD.status IN ('queued','retry_wait') THEN 1 ELSE 0 END THEN RAISE(ABORT,'invalid command attempt counter') END;
END;
CREATE TRIGGER rename_delete_terminal BEFORE DELETE ON rename_commands WHEN OLD.status IN ('queued','running','retry_wait')
BEGIN SELECT RAISE(ABORT,'active commands cannot be deleted'); END;

-- Every existing pool admission also counts active rename commands (kept in step with the
-- standing pool guard in tests/schema_migrations.rs).
DROP TRIGGER commands_admit;
CREATE TRIGGER commands_admit BEFORE INSERT ON commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM recycle_bin_cleanup_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rename_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.provider_id AND p.revision=NEW.provider_revision AND p.enabled=1 AND p.implementation IN (SELECT implementation FROM provider_implementations WHERE kind='download_client') AND s.media_type=NEW.media_type) THEN RAISE(ABORT,'invalid refresh provider scope or revision') END;
END;
DROP TRIGGER metadata_refresh_admit;
CREATE TRIGGER metadata_refresh_admit BEFORE INSERT ON metadata_refresh_commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM recycle_bin_cleanup_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rename_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM series s WHERE s.id=NEW.series_id AND s.tvdb_id=NEW.external_id) AND NOT EXISTS(SELECT 1 FROM movies m JOIN movie_metadata d ON d.id=m.metadata_id WHERE m.id=NEW.movie_id AND m.metadata_id=NEW.metadata_id AND d.tmdb_id=NEW.external_id) THEN RAISE(ABORT,'invalid metadata refresh target') END;
END;
DROP TRIGGER blocklist_clear_admit;
CREATE TRIGGER blocklist_clear_admit BEFORE INSERT ON blocklist_clear_commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM recycle_bin_cleanup_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rename_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
END;
DROP TRIGGER rss_commands_admit;
CREATE TRIGGER rss_commands_admit BEFORE INSERT ON rss_commands BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM recycle_bin_cleanup_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rename_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 OR NEW.fetch_complete!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT (EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.indexer_id AND p.revision=NEW.indexer_revision AND p.implementation IN ('torznab','newznab') AND s.media_type=NEW.media_type AND p.enabled=1) AND (NEW.selection_kind='default' OR EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.client_id AND p.revision=NEW.client_revision AND p.implementation IN (SELECT c.implementation FROM provider_implementations c JOIN provider_implementations i ON i.protocol=c.protocol JOIN providers x ON x.implementation=i.implementation WHERE c.kind='download_client' AND x.id=NEW.indexer_id) AND s.media_type=NEW.media_type AND p.enabled=1))) THEN RAISE(ABORT,'invalid RSS provider scope or revision') END;
END;
DROP TRIGGER search_commands_admit;
CREATE TRIGGER search_commands_admit BEFORE INSERT ON search_commands BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM recycle_bin_cleanup_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rename_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 OR NEW.fetch_complete!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT (EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.indexer_id AND p.revision=NEW.indexer_revision AND p.implementation IN ('torznab','newznab') AND s.media_type=NEW.media_type AND p.enabled=1) AND (NEW.selection_kind='default' OR EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.client_id AND p.revision=NEW.client_revision AND p.implementation IN (SELECT c.implementation FROM provider_implementations c JOIN provider_implementations i ON i.protocol=c.protocol JOIN providers x ON x.implementation=i.implementation WHERE c.kind='download_client' AND x.id=NEW.indexer_id) AND s.media_type=NEW.media_type AND p.enabled=1))) THEN RAISE(ABORT,'invalid RSS provider scope or revision') END;
 SELECT CASE WHEN NOT ((NEW.media_type='tv' AND EXISTS(SELECT 1 FROM episodes e WHERE e.id=NEW.requested_episode_id AND json_extract(NEW.captured_target_json,'$.media_type')='tv' AND json_extract(NEW.captured_target_json,'$.episode_id')=e.id AND json_extract(NEW.captured_target_json,'$.series_id')=e.series_id)) OR (NEW.media_type='movies' AND EXISTS(SELECT 1 FROM movies m WHERE m.id=NEW.requested_movie_id AND json_extract(NEW.captured_target_json,'$.media_type')='movies' AND json_extract(NEW.captured_target_json,'$.movie_id')=m.id AND json_extract(NEW.captured_target_json,'$.metadata_id')=m.metadata_id))) THEN RAISE(ABORT,'invalid search target') END;
END;
DROP TRIGGER manual_import_commands_admit;
CREATE TRIGGER manual_import_commands_admit BEFORE INSERT ON manual_import_commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM recycle_bin_cleanup_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rename_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM import_journal WHERE operation_id=NEW.operation_id) THEN RAISE(ABORT,'operation has no import journal') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM rss_candidate_imports WHERE operation_id=NEW.operation_id) THEN RAISE(ABORT,'operation is owned by automated download import') END;
END;
DROP TRIGGER quality_reset_admit;
CREATE TRIGGER quality_reset_admit BEFORE INSERT ON quality_reset_commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM recycle_bin_cleanup_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rename_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
END;
DROP TRIGGER rescan_admit;
CREATE TRIGGER rescan_admit BEFORE INSERT ON rescan_commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM recycle_bin_cleanup_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rename_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT ((NEW.media_type='tv' AND EXISTS(SELECT 1 FROM series WHERE id=NEW.series_id)) OR (NEW.media_type='movies' AND EXISTS(SELECT 1 FROM movies WHERE id=NEW.movie_id))) THEN RAISE(ABORT,'invalid rescan target') END;
 -- 'preview' does not touch the filesystem yet and may never be executed, so it alone must not
 -- block a rescan forever; 'complete' is done. The window that matters is real transfer work.
 SELECT CASE WHEN EXISTS(SELECT 1 FROM import_journal j JOIN operations o ON o.id=j.operation_id WHERE j.phase NOT IN ('preview','complete') AND ((NEW.media_type='tv' AND o.media_type='episode' AND EXISTS(SELECT 1 FROM episodes e WHERE e.id=o.episode_id AND e.series_id=NEW.series_id)) OR (NEW.media_type='movies' AND o.media_type='movie' AND o.movie_id=NEW.movie_id))) THEN RAISE(ABORT,'rescan target has an in-flight import') END;
 -- 'queued' has not started preflight/transfer work; 'blocked'/'cancelled' are paused, not active.
 SELECT CASE WHEN EXISTS(SELECT 1 FROM download_processing dp JOIN rss_candidates r ON r.id=dp.candidate_id WHERE dp.status IN ('checking','importing') AND ((NEW.media_type='tv' AND r.media_type='tv' AND r.series_id=NEW.series_id) OR (NEW.media_type='movies' AND r.media_type='movies' AND r.movie_id=NEW.movie_id))) THEN RAISE(ABORT,'rescan target has in-flight download processing') END;
END;
DROP TRIGGER health_admit;
CREATE TRIGGER health_admit BEFORE INSERT ON health_commands BEGIN SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM recycle_bin_cleanup_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rename_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END; SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 OR NEW.epoch IS NOT (SELECT epoch FROM health_lifecycle WHERE id=1) THEN RAISE(ABORT,'invalid health admission') END; END;
DROP TRIGGER downloaded_scan_admit;
CREATE TRIGGER downloaded_scan_admit BEFORE INSERT ON downloaded_scan_commands BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM recycle_bin_cleanup_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rename_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 OR NEW.run_generation!=0 OR NEW.cancel_requested!=0 OR NEW.inventory_revision!=0 OR NEW.inventory_count!=0 OR NEW.inventory_bytes!=0 OR NEW.inventory_json IS NOT NULL OR NEW.candidate_id IS NOT NULL OR NEW.cleanup_json IS NOT NULL OR NEW.cleanup_resume_generation!=0 OR NEW.cleanup_resume_requested!=0 OR NEW.ready_order_at!=NEW.created_at THEN RAISE(ABORT,'scan must be queued without resolved authority') END;
 SELECT CASE WHEN NEW.supersedes_command_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM downloaded_scan_commands p WHERE p.id=NEW.supersedes_command_id AND p.media_type=NEW.media_type AND p.source_path=NEW.source_path AND p.status IN ('succeeded','failed','cancelled') AND p.coverage_state='incomplete' AND NOT EXISTS(SELECT 1 FROM downloaded_scan_reserved_children c WHERE c.command_id=p.id)) THEN RAISE(ABORT,'scan successor requires settled incomplete predecessor') END;
END;
DROP TRIGGER recycle_bin_cleanup_admit;
CREATE TRIGGER recycle_bin_cleanup_admit BEFORE INSERT ON recycle_bin_cleanup_commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM recycle_bin_cleanup_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rename_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 OR NEW.outcome IS NOT NULL OR NEW.bin_path IS NOT NULL OR NEW.cleanup_days IS NOT NULL THEN RAISE(ABORT,'commands must be enqueued') END;
END;
-- Rename notifications. One event per renamed series or movie when its command settles, listing the
-- paths it changed; the payload is bounded to the event table's 16 KiB cap (the rest are counted in
-- rename_events, which stays the complete record), so one oversized path set cannot abort the
-- status update that settles the command.
CREATE TRIGGER notification_event_rename_tv AFTER UPDATE OF status ON rename_commands
WHEN NEW.media_type='tv' AND NEW.status IN ('succeeded','failed','cancelled') AND OLD.status NOT IN ('succeeded','failed','cancelled') AND NEW.items_renamed>0
 AND EXISTS(SELECT 1 FROM notification_scopes s JOIN notifications n ON n.id=s.notification_id WHERE n.enabled=1 AND s.media_type='tv' AND s.on_rename=1)
BEGIN
 INSERT INTO notification_events(kind,scope,subject_id,payload_json,created_at)
 SELECT 'rename','tv',series_id,json_object('paths',json_group_array(new_path),'previous_paths',json_group_array(old_path)),CAST(strftime('%s','now') AS INTEGER)
 FROM (SELECT series_id,old_path,new_path FROM (
   SELECT series_id,old_path,new_path,rowid,
    SUM(length(CAST(json_quote(old_path) AS BLOB))+length(CAST(json_quote(new_path) AS BLOB))+8) OVER (PARTITION BY series_id ORDER BY rowid) running
   FROM rename_events WHERE command_id=NEW.id AND media_type='tv')
  WHERE running<=15000 ORDER BY series_id,rowid)
 GROUP BY series_id;
END;
CREATE TRIGGER notification_event_rename_movies AFTER UPDATE OF status ON rename_commands
WHEN NEW.media_type='movies' AND NEW.status IN ('succeeded','failed','cancelled') AND OLD.status NOT IN ('succeeded','failed','cancelled') AND NEW.items_renamed>0
 AND EXISTS(SELECT 1 FROM notification_scopes s JOIN notifications n ON n.id=s.notification_id WHERE n.enabled=1 AND s.media_type='movies' AND s.on_rename=1)
BEGIN
 INSERT INTO notification_events(kind,scope,subject_id,payload_json,created_at)
 SELECT 'rename','movies',movie_id,json_object('paths',json_group_array(new_path),'previous_paths',json_group_array(old_path)),CAST(strftime('%s','now') AS INTEGER)
 FROM (SELECT movie_id,old_path,new_path FROM (
   SELECT movie_id,old_path,new_path,rowid,
    SUM(length(CAST(json_quote(old_path) AS BLOB))+length(CAST(json_quote(new_path) AS BLOB))+8) OVER (PARTITION BY movie_id ORDER BY rowid) running
   FROM rename_events WHERE command_id=NEW.id AND media_type='movies')
  WHERE running<=15000 ORDER BY movie_id,rowid)
 GROUP BY movie_id;
END;
