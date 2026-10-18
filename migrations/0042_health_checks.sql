-- Health observations are diagnostic state, never CDH permission authority.
-- Zero lifecycle timestamps and NULL observations mean never started/evaluated.
CREATE TABLE health_lifecycle (
 id INTEGER PRIMARY KEY CHECK(id=1),
 epoch TEXT NOT NULL CHECK(length(epoch)=36 AND substr(epoch,9,1)='-' AND substr(epoch,14,1)='-' AND substr(epoch,19,1)='-' AND substr(epoch,24,1)='-' AND length(replace(epoch,'-',''))=32 AND replace(epoch,'-','') NOT GLOB '*[^0-9a-f]*'),
 started_at INTEGER NOT NULL CHECK(typeof(started_at)='integer' AND started_at BETWEEN 0 AND 9007199254740991),
 grace_due_at INTEGER NOT NULL CHECK(typeof(grace_due_at)='integer' AND grace_due_at BETWEEN 0 AND 9007199254740991),
 grace_phase TEXT NOT NULL CHECK(grace_phase IN ('pending','rechecking','expired')),
 next_scheduled_at INTEGER NOT NULL CHECK(typeof(next_scheduled_at)='integer' AND next_scheduled_at BETWEEN 0 AND 9007199254740991),
 schedule_error TEXT CHECK(schedule_error IS NULL OR schedule_error IN ('command_history_full','storage_error','health_invariant')),
 last_batch_completed_at INTEGER CHECK(last_batch_completed_at IS NULL OR (typeof(last_batch_completed_at)='integer' AND last_batch_completed_at BETWEEN 0 AND 9007199254740991)),
 CHECK(grace_due_at>=started_at),
 CHECK(grace_phase!='expired' OR (last_batch_completed_at IS NOT NULL AND last_batch_completed_at>=grace_due_at))
);
INSERT INTO health_lifecycle(id,epoch,started_at,grace_due_at,grace_phase,next_scheduled_at)VALUES(1,'00000000-0000-0000-0000-000000000000',0,0,'pending',0);
CREATE TRIGGER health_lifecycle_keep BEFORE DELETE ON health_lifecycle BEGIN SELECT RAISE(ABORT,'health lifecycle is required'); END;
CREATE TABLE health_checks (
 scope TEXT NOT NULL CHECK(scope IN ('tv','movies','system')),
 check_key TEXT NOT NULL CHECK(length(CAST(check_key AS BLOB)) BETWEEN 1 AND 64 AND check_key NOT GLOB '*[^a-z0-9_]*' AND substr(check_key,1,1) GLOB '[a-z]'),
 startup INTEGER NOT NULL CHECK(typeof(startup)='integer' AND startup BETWEEN 0 AND 1),
 scheduled INTEGER NOT NULL CHECK(typeof(scheduled)='integer' AND scheduled BETWEEN 0 AND 1),
 generation INTEGER NOT NULL DEFAULT 0 CHECK(typeof(generation)='integer' AND generation BETWEEN 0 AND 9007199254740991),
 due_at INTEGER CHECK(due_at IS NULL OR (typeof(due_at)='integer' AND due_at BETWEEN 0 AND 9007199254740991)),
 pending_reasons INTEGER NOT NULL DEFAULT 0 CHECK(typeof(pending_reasons)='integer' AND pending_reasons BETWEEN 0 AND 31),
 observed_generation INTEGER CHECK(observed_generation IS NULL OR (typeof(observed_generation)='integer' AND observed_generation BETWEEN 0 AND 9007199254740991)),
 observed_epoch TEXT CHECK(observed_epoch IS NULL OR (length(observed_epoch)=36 AND substr(observed_epoch,9,1)='-' AND substr(observed_epoch,14,1)='-' AND substr(observed_epoch,19,1)='-' AND substr(observed_epoch,24,1)='-' AND length(replace(observed_epoch,'-',''))=32 AND replace(observed_epoch,'-','') NOT GLOB '*[^0-9a-f]*')),
 checked_at INTEGER CHECK(checked_at IS NULL OR (typeof(checked_at)='integer' AND checked_at BETWEEN 0 AND 9007199254740991)),
 last_error TEXT CHECK(last_error IS NULL OR last_error IN ('interrupted','storage_error','check_failed','check_timeout','stale_inputs','cancelled')),
 severity INTEGER CHECK(severity IS NULL OR (typeof(severity)='integer' AND severity BETWEEN 0 AND 3)),
 reason TEXT CHECK(reason IS NULL OR (typeof(reason)='text' AND length(CAST(reason AS BLOB)) BETWEEN 1 AND 128)),
 message TEXT CHECK(message IS NULL OR (typeof(message)='text' AND length(CAST(message AS BLOB)) BETWEEN 1 AND 4096)),
 wiki_url TEXT CHECK(wiki_url IS NULL OR (typeof(wiki_url)='text' AND length(CAST(wiki_url AS BLOB)) BETWEEN 1 AND 2048)),
 compatibility_type TEXT NOT NULL CHECK(typeof(compatibility_type)='text' AND length(CAST(compatibility_type AS BLOB)) BETWEEN 1 AND 128),
 PRIMARY KEY(scope,check_key),
 CHECK((pending_reasons=0 AND due_at IS NULL) OR (pending_reasons>0 AND due_at IS NOT NULL)),
 CHECK(observed_generation IS NULL OR observed_generation<=generation),
 CHECK((severity IS NULL AND observed_generation IS NULL AND observed_epoch IS NULL AND checked_at IS NULL) OR (severity IS NOT NULL AND observed_generation IS NOT NULL AND observed_epoch IS NOT NULL AND checked_at IS NOT NULL)),
 CHECK(((severity IS NULL OR severity=0) AND reason IS NULL AND message IS NULL AND wiki_url IS NULL) OR (severity IS NOT NULL AND severity>0 AND reason IS NOT NULL AND message IS NOT NULL AND wiki_url IS NOT NULL))
);
CREATE TRIGGER health_checks_bound BEFORE INSERT ON health_checks WHEN (SELECT count(*) FROM health_checks)>=128 BEGIN SELECT RAISE(ABORT,'health registry capacity reached'); END;
CREATE TRIGGER health_checks_identity BEFORE UPDATE ON health_checks BEGIN SELECT CASE WHEN NEW.scope IS NOT OLD.scope OR NEW.check_key IS NOT OLD.check_key OR NEW.generation<OLD.generation OR NEW.generation>OLD.generation+1 THEN RAISE(ABORT,'invalid health identity or generation') END; END;
INSERT INTO health_checks(scope,check_key,startup,scheduled,compatibility_type)VALUES('tv','completed_download_handling',1,1,'ImportMechanismCheck'),('movies','completed_download_handling',1,1,'ImportMechanismCheck');
CREATE TABLE health_commands (
 id TEXT NOT NULL CHECK(length(id)=36 AND substr(id,9,1)='-' AND substr(id,14,1)='-' AND substr(id,19,1)='-' AND substr(id,24,1)='-' AND length(replace(id,'-',''))=32 AND replace(id,'-','') NOT GLOB '*[^0-9a-f]*'),
 scope TEXT NOT NULL DEFAULT 'all' CHECK(scope='all'),
 priority INTEGER NOT NULL DEFAULT 0 CHECK(typeof(priority)='integer' AND priority BETWEEN 0 AND 1),
 status TEXT NOT NULL DEFAULT 'queued' CHECK(status IN ('queued','running','retry_wait','succeeded','failed','cancelled')),
 attempts INTEGER NOT NULL DEFAULT 0 CHECK(typeof(attempts)='integer' AND attempts BETWEEN 0 AND 3),
 next_attempt_at INTEGER NOT NULL CHECK(typeof(next_attempt_at)='integer' AND next_attempt_at BETWEEN 0 AND 9007199254740991),
 created_at INTEGER NOT NULL CHECK(typeof(created_at)='integer' AND created_at BETWEEN 0 AND 9007199254740991),
 started_at INTEGER CHECK(started_at IS NULL OR (typeof(started_at)='integer' AND started_at BETWEEN 0 AND 9007199254740991)),
 completed_at INTEGER CHECK(completed_at IS NULL OR (typeof(completed_at)='integer' AND completed_at BETWEEN 0 AND 9007199254740991)),
 error_code TEXT CHECK(error_code IS NULL OR error_code IN ('interrupted','storage_error','check_failed','check_timeout','stale_inputs','cancelled')),
 epoch TEXT NOT NULL CHECK(length(epoch)=36 AND substr(epoch,9,1)='-' AND substr(epoch,14,1)='-' AND substr(epoch,19,1)='-' AND substr(epoch,24,1)='-' AND length(replace(epoch,'-',''))=32 AND replace(epoch,'-','') NOT GLOB '*[^0-9a-f]*'),
 is_grace INTEGER NOT NULL DEFAULT 0 CHECK(typeof(is_grace)='integer' AND is_grace BETWEEN 0 AND 1),
 PRIMARY KEY(id),
 CHECK((status IN ('queued','running','retry_wait') AND completed_at IS NULL) OR (status IN ('succeeded','failed','cancelled') AND completed_at IS NOT NULL)),
 CHECK(status!='queued' OR (attempts=0 AND started_at IS NULL)),
 CHECK(status NOT IN ('running','succeeded','retry_wait') OR (attempts>0 AND started_at IS NOT NULL)),
 CHECK(status!='retry_wait' OR attempts<3),
 CHECK((status IN ('failed','retry_wait') AND error_code IS NOT NULL) OR (status NOT IN ('failed','retry_wait') AND error_code IS NULL))
);
CREATE UNIQUE INDEX health_active_batch ON health_commands((1)) WHERE status IN ('queued','running','retry_wait');
CREATE INDEX health_ready ON health_commands(status,next_attempt_at,priority,created_at,id);
CREATE INDEX health_history ON health_commands(completed_at,id);
CREATE TABLE health_command_checks (
 command_id TEXT NOT NULL REFERENCES health_commands(id) ON DELETE CASCADE,
 scope TEXT NOT NULL,
 check_key TEXT NOT NULL,
 admitted_generation INTEGER NOT NULL CHECK(typeof(admitted_generation)='integer' AND admitted_generation BETWEEN 0 AND 9007199254740991),
 captured_generation INTEGER CHECK(captured_generation IS NULL OR (typeof(captured_generation)='integer' AND captured_generation BETWEEN 0 AND 9007199254740991)),
 captured_reasons INTEGER CHECK(captured_reasons IS NULL OR (typeof(captured_reasons)='integer' AND captured_reasons BETWEEN 1 AND 31)),
 captured_due_at INTEGER CHECK(captured_due_at IS NULL OR (typeof(captured_due_at)='integer' AND captured_due_at BETWEEN 0 AND 9007199254740991)),
 PRIMARY KEY(command_id,scope,check_key),
 FOREIGN KEY(scope,check_key) REFERENCES health_checks(scope,check_key) ON DELETE RESTRICT,
 CHECK((captured_generation IS NULL AND captured_reasons IS NULL AND captured_due_at IS NULL) OR (captured_generation IS NOT NULL AND captured_reasons IS NOT NULL AND captured_due_at IS NOT NULL)),
 CHECK(captured_generation IS NULL OR captured_generation>=admitted_generation)
);
CREATE TRIGGER health_members_admit BEFORE INSERT ON health_command_checks BEGIN
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM health_commands WHERE id=NEW.command_id AND status='queued' AND attempts=0) THEN RAISE(ABORT,'health membership is frozen') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM health_checks WHERE scope=NEW.scope AND check_key=NEW.check_key AND generation=NEW.admitted_generation) OR NEW.captured_generation IS NOT NULL THEN RAISE(ABORT,'invalid health admission generation') END;
END;
CREATE TRIGGER health_members_update BEFORE UPDATE ON health_command_checks BEGIN
 SELECT CASE WHEN NEW.command_id IS NOT OLD.command_id OR NEW.scope IS NOT OLD.scope OR NEW.check_key IS NOT OLD.check_key THEN RAISE(ABORT,'health membership identity is immutable') END;
 SELECT CASE WHEN NEW.admitted_generation IS NOT OLD.admitted_generation AND NOT EXISTS(SELECT 1 FROM health_commands WHERE id=OLD.command_id AND status='queued' AND attempts=0) THEN RAISE(ABORT,'health admission ownership is frozen') END;
 SELECT CASE WHEN NEW.admitted_generation<OLD.admitted_generation OR NOT EXISTS(SELECT 1 FROM health_checks WHERE scope=NEW.scope AND check_key=NEW.check_key AND generation>=NEW.admitted_generation AND (NEW.captured_generation IS NULL OR generation>=NEW.captured_generation)) THEN RAISE(ABORT,'invalid health member generation') END;
 SELECT CASE WHEN (NEW.captured_generation IS NOT OLD.captured_generation OR NEW.captured_reasons IS NOT OLD.captured_reasons OR NEW.captured_due_at IS NOT OLD.captured_due_at) AND NOT EXISTS(SELECT 1 FROM health_commands WHERE id=OLD.command_id AND status='running') THEN RAISE(ABORT,'health attempt capture requires running command') END;
END;
CREATE TRIGGER health_members_delete BEFORE DELETE ON health_command_checks WHEN EXISTS(SELECT 1 FROM health_commands WHERE id=OLD.command_id AND status IN ('queued','running','retry_wait')) BEGIN SELECT RAISE(ABORT,'active health membership cannot be deleted'); END;
CREATE TABLE health_transitions (
 sequence INTEGER PRIMARY KEY AUTOINCREMENT CHECK(typeof(sequence)='integer' AND sequence BETWEEN 1 AND 9007199254740991),
 event_id TEXT NOT NULL CHECK(length(event_id)=36 AND substr(event_id,9,1)='-' AND substr(event_id,14,1)='-' AND substr(event_id,19,1)='-' AND substr(event_id,24,1)='-' AND length(replace(event_id,'-',''))=32 AND replace(event_id,'-','') NOT GLOB '*[^0-9a-f]*'),
 epoch TEXT NOT NULL CHECK(length(epoch)=36 AND substr(epoch,9,1)='-' AND substr(epoch,14,1)='-' AND substr(epoch,19,1)='-' AND substr(epoch,24,1)='-' AND length(replace(epoch,'-',''))=32 AND replace(epoch,'-','') NOT GLOB '*[^0-9a-f]*'),
 command_id TEXT NOT NULL CHECK(length(command_id)=36 AND substr(command_id,9,1)='-' AND substr(command_id,14,1)='-' AND substr(command_id,19,1)='-' AND substr(command_id,24,1)='-' AND length(replace(command_id,'-',''))=32 AND replace(command_id,'-','') NOT GLOB '*[^0-9a-f]*'),
 scope TEXT NOT NULL CHECK(scope IN ('tv','movies','system')),
 check_key TEXT NOT NULL CHECK(length(CAST(check_key AS BLOB)) BETWEEN 1 AND 64 AND check_key NOT GLOB '*[^a-z0-9_]*' AND substr(check_key,1,1) GLOB '[a-z]'),
 kind TEXT NOT NULL CHECK(kind IN ('issue','restored')),
 in_grace INTEGER NOT NULL CHECK(typeof(in_grace)='integer' AND in_grace BETWEEN 0 AND 1),
 created_at INTEGER NOT NULL CHECK(typeof(created_at)='integer' AND created_at BETWEEN 0 AND 9007199254740991),
 severity INTEGER NOT NULL CHECK(typeof(severity)='integer' AND severity BETWEEN 1 AND 3),
 reason TEXT NOT NULL CHECK(typeof(reason)='text' AND length(CAST(reason AS BLOB)) BETWEEN 1 AND 128),
 message TEXT NOT NULL CHECK(typeof(message)='text' AND length(CAST(message AS BLOB)) BETWEEN 1 AND 4096),
 wiki_url TEXT NOT NULL CHECK(typeof(wiki_url)='text' AND length(CAST(wiki_url AS BLOB)) BETWEEN 1 AND 2048),
 compatibility_type TEXT NOT NULL CHECK(typeof(compatibility_type)='text' AND length(CAST(compatibility_type AS BLOB)) BETWEEN 1 AND 128),
 UNIQUE(event_id),
 UNIQUE(command_id,scope,check_key,kind,in_grace)
);
CREATE TRIGGER health_transition_immutable BEFORE UPDATE ON health_transitions BEGIN SELECT RAISE(ABORT,'health transition is immutable'); END;
-- The bounded diagnostic ring is not a reliable external delivery outbox.
CREATE TRIGGER health_transition_retention AFTER INSERT ON health_transitions BEGIN DELETE FROM health_transitions WHERE sequence IN (SELECT sequence FROM health_transitions ORDER BY sequence DESC LIMIT -1 OFFSET 1024); END;
CREATE TRIGGER health_admit BEFORE INSERT ON health_commands BEGIN SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END; SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 OR NEW.epoch IS NOT (SELECT epoch FROM health_lifecycle WHERE id=1) THEN RAISE(ABORT,'invalid health admission') END; END;
CREATE TRIGGER health_command_transition BEFORE UPDATE ON health_commands BEGIN
 SELECT CASE WHEN NEW.id IS NOT OLD.id OR NEW.scope IS NOT OLD.scope OR NEW.created_at IS NOT OLD.created_at THEN RAISE(ABORT,'health command identity is immutable') END;
 SELECT CASE WHEN NOT ((OLD.status='queued' AND NEW.status IN ('queued','running','cancelled','failed')) OR (OLD.status='retry_wait' AND NEW.status IN ('running','cancelled','failed')) OR (OLD.status='running' AND NEW.status IN ('retry_wait','succeeded','failed','cancelled'))) THEN RAISE(ABORT,'invalid health command transition') END;
 SELECT CASE WHEN NEW.attempts!=OLD.attempts+CASE WHEN OLD.status IN ('queued','retry_wait') AND NEW.status='running' THEN 1 ELSE 0 END THEN RAISE(ABORT,'invalid health attempt counter') END;
 SELECT CASE WHEN NEW.priority IS NOT OLD.priority AND NOT (OLD.status='queued' AND NEW.status='queued' AND NEW.priority>OLD.priority) THEN RAISE(ABORT,'health priority is frozen') END;
 SELECT CASE WHEN NEW.is_grace IS NOT OLD.is_grace AND NOT (OLD.status='queued' AND NEW.status='queued' AND OLD.is_grace=0 AND NEW.is_grace=1) THEN RAISE(ABORT,'health grace ownership is frozen') END;
 SELECT CASE WHEN NEW.epoch IS NOT OLD.epoch AND NOT (OLD.status IN ('queued','retry_wait') AND NEW.status='running' AND NEW.is_grace=0 AND NEW.epoch IS (SELECT epoch FROM health_lifecycle WHERE id=1)) THEN RAISE(ABORT,'invalid health epoch claim') END;
 SELECT CASE WHEN NEW.status='running' AND (NEW.epoch IS NOT (SELECT epoch FROM health_lifecycle WHERE id=1) OR NOT EXISTS(SELECT 1 FROM health_command_checks WHERE command_id=NEW.id)) THEN RAISE(ABORT,'invalid health claim') END;
 SELECT CASE WHEN NEW.status='running' AND NEW.is_grace=1 AND NOT EXISTS(SELECT 1 FROM health_lifecycle WHERE id=1 AND epoch=NEW.epoch AND grace_due_at<=NEW.started_at AND grace_phase='rechecking') THEN RAISE(ABORT,'grace claim is not due') END;
 SELECT CASE WHEN NEW.status='running' AND NEW.is_grace=1 AND EXISTS(SELECT 1 FROM health_checks h WHERE h.startup=1 AND NOT EXISTS(SELECT 1 FROM health_command_checks m WHERE m.command_id=NEW.id AND m.scope=h.scope AND m.check_key=h.check_key)) THEN RAISE(ABORT,'grace claim requires every startup check') END;
END;
CREATE TRIGGER health_command_delete BEFORE DELETE ON health_commands WHEN OLD.status IN ('queued','running','retry_wait') BEGIN SELECT RAISE(ABORT,'active health commands cannot be deleted'); END;
CREATE TRIGGER health_command_retention_insert AFTER INSERT ON health_commands BEGIN DELETE FROM health_commands WHERE id IN (SELECT id FROM health_commands WHERE status IN ('succeeded','failed','cancelled') ORDER BY completed_at DESC,id DESC LIMIT -1 OFFSET 128); END;
CREATE TRIGGER health_command_retention_finish AFTER UPDATE OF status ON health_commands WHEN NEW.status IN ('succeeded','failed','cancelled') BEGIN DELETE FROM health_commands WHERE id IN (SELECT id FROM health_commands WHERE status IN ('succeeded','failed','cancelled') ORDER BY completed_at DESC,id DESC LIMIT -1 OFFSET 128); END;
-- Preserve every existing target/provider/state predicate; add only health to active capacity.
DROP TRIGGER commands_admit;
CREATE TRIGGER commands_admit BEFORE INSERT ON commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.provider_id AND p.revision=NEW.provider_revision AND p.enabled=1 AND p.implementation='qbittorrent' AND s.media_type=NEW.media_type) THEN RAISE(ABORT,'invalid refresh provider scope or revision') END;
END;
DROP TRIGGER metadata_refresh_admit;
CREATE TRIGGER metadata_refresh_admit BEFORE INSERT ON metadata_refresh_commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM series s WHERE s.id=NEW.series_id AND s.tvdb_id=NEW.external_id) AND NOT EXISTS(SELECT 1 FROM movies m JOIN movie_metadata d ON d.id=m.metadata_id WHERE m.id=NEW.movie_id AND m.metadata_id=NEW.metadata_id AND d.tmdb_id=NEW.external_id) THEN RAISE(ABORT,'invalid metadata refresh target') END;
END;
DROP TRIGGER blocklist_clear_admit;
CREATE TRIGGER blocklist_clear_admit BEFORE INSERT ON blocklist_clear_commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
END;
DROP TRIGGER rss_commands_admit;
CREATE TRIGGER rss_commands_admit BEFORE INSERT ON rss_commands BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 OR NEW.fetch_complete!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT (EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.indexer_id AND p.revision=NEW.indexer_revision AND p.implementation IN ('torznab','newznab') AND s.media_type=NEW.media_type AND p.enabled=1) AND EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.client_id AND p.revision=NEW.client_revision AND p.implementation='qbittorrent' AND s.media_type=NEW.media_type AND p.enabled=1)) THEN RAISE(ABORT,'invalid RSS provider scope or revision') END;
END;
DROP TRIGGER search_commands_admit;
CREATE TRIGGER search_commands_admit BEFORE INSERT ON search_commands BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 OR NEW.fetch_complete!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT (EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.indexer_id AND p.revision=NEW.indexer_revision AND p.implementation IN ('torznab','newznab') AND s.media_type=NEW.media_type AND p.enabled=1) AND EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.client_id AND p.revision=NEW.client_revision AND p.implementation='qbittorrent' AND s.media_type=NEW.media_type AND p.enabled=1)) THEN RAISE(ABORT,'invalid RSS provider scope or revision') END;
 SELECT CASE WHEN NOT ((NEW.media_type='tv' AND EXISTS(SELECT 1 FROM episodes e WHERE e.id=NEW.requested_episode_id AND json_extract(NEW.captured_target_json,'$.media_type')='tv' AND json_extract(NEW.captured_target_json,'$.episode_id')=e.id AND json_extract(NEW.captured_target_json,'$.series_id')=e.series_id)) OR (NEW.media_type='movies' AND EXISTS(SELECT 1 FROM movies m WHERE m.id=NEW.requested_movie_id AND json_extract(NEW.captured_target_json,'$.media_type')='movies' AND json_extract(NEW.captured_target_json,'$.movie_id')=m.id AND json_extract(NEW.captured_target_json,'$.metadata_id')=m.metadata_id))) THEN RAISE(ABORT,'invalid search target') END;
END;
DROP TRIGGER manual_import_commands_admit;
CREATE TRIGGER manual_import_commands_admit BEFORE INSERT ON manual_import_commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM import_journal WHERE operation_id=NEW.operation_id) THEN RAISE(ABORT,'operation has no import journal') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM rss_candidate_imports WHERE operation_id=NEW.operation_id) THEN RAISE(ABORT,'operation is owned by automated download import') END;
END;
DROP TRIGGER quality_reset_admit;
CREATE TRIGGER quality_reset_admit BEFORE INSERT ON quality_reset_commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
END;
DROP TRIGGER rescan_admit;
CREATE TRIGGER rescan_admit BEFORE INSERT ON rescan_commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT ((NEW.media_type='tv' AND EXISTS(SELECT 1 FROM series WHERE id=NEW.series_id)) OR (NEW.media_type='movies' AND EXISTS(SELECT 1 FROM movies WHERE id=NEW.movie_id))) THEN RAISE(ABORT,'invalid rescan target') END;
 -- 'preview' does not touch the filesystem yet and may never be executed, so it alone must not
 -- block a rescan forever; 'complete' is done. The window that matters is real transfer work.
 SELECT CASE WHEN EXISTS(SELECT 1 FROM import_journal j JOIN operations o ON o.id=j.operation_id WHERE j.phase NOT IN ('preview','complete') AND ((NEW.media_type='tv' AND o.media_type='episode' AND EXISTS(SELECT 1 FROM episodes e WHERE e.id=o.episode_id AND e.series_id=NEW.series_id)) OR (NEW.media_type='movies' AND o.media_type='movie' AND o.movie_id=NEW.movie_id))) THEN RAISE(ABORT,'rescan target has an in-flight import') END;
 -- 'queued' has not started preflight/transfer work; 'blocked'/'cancelled' are paused, not active.
 SELECT CASE WHEN EXISTS(SELECT 1 FROM download_processing dp JOIN rss_candidates r ON r.id=dp.candidate_id WHERE dp.status IN ('checking','importing') AND ((NEW.media_type='tv' AND r.media_type='tv' AND r.series_id=NEW.series_id) OR (NEW.media_type='movies' AND r.media_type='movies' AND r.movie_id=NEW.movie_id))) THEN RAISE(ABORT,'rescan target has in-flight download processing') END;
END;

