-- Concrete, read-only qBittorrent refresh work. Historical commands survive provider deletion.
-- This does not authorize grabs, imports, or filename-derived library associations.
CREATE TABLE commands (
 id TEXT PRIMARY KEY NOT NULL CHECK(length(id)=36 AND substr(id,9,1)='-' AND substr(id,14,1)='-' AND substr(id,19,1)='-' AND substr(id,24,1)='-' AND length(replace(id,'-',''))=32 AND replace(id,'-','') NOT GLOB '*[^0-9a-f]*'),
 name TEXT NOT NULL DEFAULT 'refresh_downloads' CHECK(name='refresh_downloads'),
 provider_id TEXT NOT NULL,
 media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
 provider_revision INTEGER NOT NULL CHECK(typeof(provider_revision)='integer' AND provider_revision BETWEEN 1 AND 9007199254740991),
 priority INTEGER NOT NULL DEFAULT 0 CHECK(typeof(priority)='integer' AND priority IN (0,1)),
 status TEXT NOT NULL DEFAULT 'queued' CHECK(status IN ('queued','running','retry_wait','succeeded','failed','cancelled')),
 attempts INTEGER NOT NULL DEFAULT 0 CHECK(typeof(attempts)='integer' AND attempts BETWEEN 0 AND 3),
 next_attempt_at INTEGER NOT NULL CHECK(typeof(next_attempt_at)='integer' AND next_attempt_at BETWEEN 0 AND 9007199254740991),
 created_at INTEGER NOT NULL CHECK(typeof(created_at)='integer' AND created_at BETWEEN 0 AND 9007199254740991),
 started_at INTEGER CHECK(started_at IS NULL OR (typeof(started_at)='integer' AND started_at BETWEEN 0 AND 9007199254740991)),
 completed_at INTEGER CHECK(completed_at IS NULL OR (typeof(completed_at)='integer' AND completed_at BETWEEN 0 AND 9007199254740991)),
 error_code TEXT CHECK(error_code IS NULL OR error_code IN ('interrupted','provider_changed','provider_unavailable','refresh_timeout','refresh_failed','refresh_limit','storage_error')),
 items_observed INTEGER NOT NULL DEFAULT 0 CHECK(typeof(items_observed)='integer' AND items_observed BETWEEN 0 AND 1000),
 CHECK((status IN ('queued','running','retry_wait') AND completed_at IS NULL) OR (status IN ('succeeded','failed','cancelled') AND completed_at IS NOT NULL)),
 CHECK(status!='queued' OR (attempts=0 AND started_at IS NULL)),
 CHECK(status NOT IN ('running','succeeded','retry_wait') OR (attempts>0 AND started_at IS NOT NULL)),
 CHECK(status!='retry_wait' OR attempts<3),
 CHECK((status IN ('failed','retry_wait') AND error_code IS NOT NULL) OR (status NOT IN ('failed','retry_wait') AND error_code IS NULL)),
 CHECK(status='succeeded' OR items_observed=0)
);
CREATE UNIQUE INDEX commands_active_scope ON commands(provider_id,media_type) WHERE status IN ('queued','running','retry_wait');
CREATE INDEX commands_ready ON commands(status,next_attempt_at,priority,created_at,id);
CREATE INDEX commands_history ON commands(created_at,id);
CREATE TRIGGER commands_admit BEFORE INSERT ON commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands)>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.provider_id AND p.revision=NEW.provider_revision AND p.enabled=1 AND p.implementation='qbittorrent' AND s.media_type=NEW.media_type) THEN RAISE(ABORT,'invalid refresh provider scope or revision') END;
END;
CREATE TRIGGER commands_transition BEFORE UPDATE ON commands
BEGIN
 SELECT CASE WHEN NEW.id IS NOT OLD.id OR NEW.name IS NOT OLD.name OR NEW.provider_id IS NOT OLD.provider_id OR NEW.media_type IS NOT OLD.media_type OR NEW.provider_revision IS NOT OLD.provider_revision OR NEW.created_at IS NOT OLD.created_at OR NEW.priority IS NOT OLD.priority THEN RAISE(ABORT,'command identity is immutable') END;
 SELECT CASE WHEN NOT ((OLD.status IN ('queued','retry_wait') AND NEW.status IN ('running','cancelled','failed')) OR (OLD.status='running' AND NEW.status IN ('running','retry_wait','succeeded','failed','cancelled'))) THEN RAISE(ABORT,'invalid command transition') END;
 SELECT CASE WHEN NEW.attempts != OLD.attempts + CASE WHEN NEW.status='running' AND OLD.status IN ('queued','retry_wait') THEN 1 ELSE 0 END THEN RAISE(ABORT,'invalid command attempt counter') END;
 SELECT CASE WHEN NEW.status IN ('running','succeeded') AND NOT EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.provider_id AND p.revision=NEW.provider_revision AND p.enabled=1 AND p.implementation='qbittorrent' AND s.media_type=NEW.media_type) THEN RAISE(ABORT,'stale refresh command') END;
END;
CREATE TRIGGER commands_delete_terminal BEFORE DELETE ON commands WHEN OLD.status IN ('queued','running','retry_wait')
BEGIN SELECT RAISE(ABORT,'active commands cannot be deleted'); END;

-- Configuration edits retain the schedule but disable it visibly; explicit provider deletion
-- removes its schedule. Scope replacement during a provider edit must not delete the schedule.
CREATE TABLE download_refresh_schedules (
 provider_id TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
 media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
 provider_revision INTEGER NOT NULL CHECK(typeof(provider_revision)='integer' AND provider_revision BETWEEN 1 AND 9007199254740991),
 enabled INTEGER NOT NULL CHECK(typeof(enabled)='integer' AND enabled IN (0,1)),
 interval_seconds INTEGER NOT NULL CHECK(typeof(interval_seconds)='integer' AND interval_seconds BETWEEN 60 AND 86400),
 next_run_at INTEGER NOT NULL CHECK(typeof(next_run_at)='integer' AND next_run_at BETWEEN 0 AND 9007199254740991),
 last_run_at INTEGER CHECK(last_run_at IS NULL OR (typeof(last_run_at)='integer' AND last_run_at BETWEEN 0 AND 9007199254740991)),
 revision INTEGER NOT NULL DEFAULT 1 CHECK(typeof(revision)='integer' AND revision BETWEEN 1 AND 9007199254740991),
 error_code TEXT CHECK(error_code IS NULL OR error_code IN ('provider_changed','command_history_full','command_conflict')),
 PRIMARY KEY(provider_id,media_type),
 CHECK(enabled=0 OR error_code IS NULL OR error_code IN ('command_history_full','command_conflict'))
);
CREATE INDEX download_refresh_schedules_due ON download_refresh_schedules(enabled,next_run_at);
CREATE TRIGGER refresh_schedule_admit BEFORE INSERT ON download_refresh_schedules
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM download_refresh_schedules)>=64 AND NOT EXISTS(SELECT 1 FROM download_refresh_schedules WHERE provider_id=NEW.provider_id AND media_type=NEW.media_type) THEN RAISE(ABORT,'refresh schedule capacity reached') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.provider_id AND p.revision=NEW.provider_revision AND p.implementation='qbittorrent' AND s.media_type=NEW.media_type AND (NEW.enabled=0 OR p.enabled=1)) THEN RAISE(ABORT,'invalid schedule provider scope or revision') END;
END;
CREATE TRIGGER refresh_schedule_update BEFORE UPDATE ON download_refresh_schedules
BEGIN
 SELECT CASE WHEN NEW.provider_id IS NOT OLD.provider_id OR NEW.media_type IS NOT OLD.media_type THEN RAISE(ABORT,'schedule identity is immutable') END;
 SELECT CASE WHEN (NEW.enabled=1 OR NEW.provider_revision IS NOT OLD.provider_revision) AND NOT EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.provider_id AND p.revision=NEW.provider_revision AND p.implementation='qbittorrent' AND s.media_type=NEW.media_type AND (NEW.enabled=0 OR p.enabled=1)) THEN RAISE(ABORT,'invalid schedule provider scope or revision') END;
END;

-- A bounded latest-success projection, not tracked library downloads. Items have no inferred
-- movie/episode target. The writer validates the closed item shape and safe numeric ranges.
CREATE TABLE download_refresh_snapshots (
 provider_id TEXT NOT NULL,
 media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
 provider_revision INTEGER NOT NULL CHECK(typeof(provider_revision)='integer' AND provider_revision BETWEEN 1 AND 9007199254740991),
 observed_at INTEGER NOT NULL CHECK(typeof(observed_at)='integer' AND observed_at BETWEEN 0 AND 9007199254740991),
 command_id TEXT REFERENCES commands(id) ON DELETE SET NULL,
 items_json TEXT NOT NULL CHECK(typeof(items_json)='text' AND length(CAST(items_json AS BLOB))<=1048576 AND json_valid(items_json) AND json_type(items_json)='array' AND json_array_length(items_json)<=1000),
 PRIMARY KEY(provider_id,media_type),
 FOREIGN KEY(provider_id,media_type) REFERENCES provider_scopes(provider_id,media_type) ON DELETE CASCADE
);
CREATE TRIGGER refresh_snapshot_insert BEFORE INSERT ON download_refresh_snapshots
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM download_refresh_snapshots)>=64 AND NOT EXISTS(SELECT 1 FROM download_refresh_snapshots WHERE provider_id=NEW.provider_id AND media_type=NEW.media_type) THEN RAISE(ABORT,'refresh snapshot capacity reached') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM providers WHERE id=NEW.provider_id AND revision=NEW.provider_revision AND enabled=1 AND implementation='qbittorrent') THEN RAISE(ABORT,'stale refresh snapshot') END;
 SELECT CASE WHEN NEW.command_id IS NULL OR NOT EXISTS(SELECT 1 FROM commands WHERE id=NEW.command_id AND provider_id=NEW.provider_id AND media_type=NEW.media_type AND provider_revision=NEW.provider_revision AND status='succeeded' AND items_observed=json_array_length(NEW.items_json)) THEN RAISE(ABORT,'snapshot command mismatch') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM json_each(NEW.items_json) WHERE type!='object' OR json_extract(value,'$.domain') IS NOT NEW.media_type OR json_type(value,'$.hash') IS NOT 'text' OR length(json_extract(value,'$.hash')) NOT IN (40,64) OR json_extract(value,'$.hash') GLOB '*[^0-9a-f]*') OR (SELECT count(*) FROM json_each(NEW.items_json))!=(SELECT count(DISTINCT json_extract(value,'$.hash')) FROM json_each(NEW.items_json)) THEN RAISE(ABORT,'invalid snapshot domain or download identity') END;
END;
CREATE TRIGGER refresh_snapshot_update BEFORE UPDATE ON download_refresh_snapshots
BEGIN
 SELECT CASE WHEN NEW.provider_id IS NOT OLD.provider_id OR NEW.media_type IS NOT OLD.media_type THEN RAISE(ABORT,'snapshot identity is immutable') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM providers WHERE id=NEW.provider_id AND revision=NEW.provider_revision AND enabled=1 AND implementation='qbittorrent') THEN RAISE(ABORT,'stale refresh snapshot') END;
 SELECT CASE WHEN NEW.command_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM commands WHERE id=NEW.command_id AND provider_id=NEW.provider_id AND media_type=NEW.media_type AND provider_revision=NEW.provider_revision AND status='succeeded' AND items_observed=json_array_length(NEW.items_json)) THEN RAISE(ABORT,'snapshot command mismatch') END;
 SELECT CASE WHEN NEW.command_id IS NULL AND (NEW.provider_revision IS NOT OLD.provider_revision OR NEW.observed_at IS NOT OLD.observed_at OR NEW.items_json IS NOT OLD.items_json) THEN RAISE(ABORT,'snapshot replacement requires a command') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM json_each(NEW.items_json) WHERE type!='object' OR json_extract(value,'$.domain') IS NOT NEW.media_type OR json_type(value,'$.hash') IS NOT 'text' OR length(json_extract(value,'$.hash')) NOT IN (40,64) OR json_extract(value,'$.hash') GLOB '*[^0-9a-f]*') OR (SELECT count(*) FROM json_each(NEW.items_json))!=(SELECT count(DISTINCT json_extract(value,'$.hash')) FROM json_each(NEW.items_json)) THEN RAISE(ABORT,'invalid snapshot domain or download identity') END;
END;
CREATE TRIGGER refresh_provider_changed AFTER UPDATE ON providers
BEGIN
 DELETE FROM download_refresh_snapshots WHERE provider_id=OLD.id;
 UPDATE download_refresh_schedules SET enabled=0,error_code='provider_changed',revision=min(revision+1,9007199254740991) WHERE provider_id=OLD.id;
END;
CREATE TRIGGER refresh_scope_insert AFTER INSERT ON provider_scopes
BEGIN
 DELETE FROM download_refresh_snapshots WHERE provider_id=NEW.provider_id AND media_type=NEW.media_type;
 UPDATE download_refresh_schedules SET enabled=0,error_code='provider_changed',revision=min(revision+1,9007199254740991) WHERE provider_id=NEW.provider_id AND media_type=NEW.media_type;
END;
CREATE TRIGGER refresh_scope_update AFTER UPDATE ON provider_scopes
BEGIN
 DELETE FROM download_refresh_snapshots WHERE (provider_id=OLD.provider_id AND media_type=OLD.media_type) OR (provider_id=NEW.provider_id AND media_type=NEW.media_type);
 UPDATE download_refresh_schedules SET enabled=0,error_code='provider_changed',revision=min(revision+1,9007199254740991) WHERE (provider_id=OLD.provider_id AND media_type=OLD.media_type) OR (provider_id=NEW.provider_id AND media_type=NEW.media_type);
END;
CREATE TRIGGER refresh_scope_delete AFTER DELETE ON provider_scopes
BEGIN
 UPDATE download_refresh_schedules SET enabled=0,error_code='provider_changed',revision=min(revision+1,9007199254740991) WHERE provider_id=OLD.provider_id AND media_type=OLD.media_type;
END;
