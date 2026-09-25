-- Concrete catalog refresh jobs share the existing worker/cap, not provider identities.
CREATE TABLE metadata_refresh_commands (
 id TEXT PRIMARY KEY NOT NULL CHECK(length(id)=36 AND substr(id,9,1)='-' AND substr(id,14,1)='-' AND substr(id,19,1)='-' AND substr(id,24,1)='-' AND length(replace(id,'-',''))=32 AND replace(id,'-','') NOT GLOB '*[^0-9a-f]*'),
 name TEXT NOT NULL CHECK(name IN ('refresh_series','refresh_movie')),
 media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
 series_id INTEGER CHECK(series_id IS NULL OR (typeof(series_id)='integer' AND series_id BETWEEN 1 AND 9007199254740991)),
 movie_id INTEGER CHECK(movie_id IS NULL OR (typeof(movie_id)='integer' AND movie_id BETWEEN 1 AND 9007199254740991)),
 external_id INTEGER NOT NULL CHECK(typeof(external_id)='integer' AND external_id BETWEEN 1 AND 9007199254740991),
 metadata_id INTEGER CHECK(metadata_id IS NULL OR (typeof(metadata_id)='integer' AND metadata_id BETWEEN 1 AND 9007199254740991)),
 priority INTEGER NOT NULL DEFAULT 0 CHECK(typeof(priority)='integer' AND priority IN (0,1)),
 status TEXT NOT NULL DEFAULT 'queued' CHECK(status IN ('queued','running','retry_wait','succeeded','failed','cancelled')),
 attempts INTEGER NOT NULL DEFAULT 0 CHECK(typeof(attempts)='integer' AND attempts BETWEEN 0 AND 3),
 next_attempt_at INTEGER NOT NULL CHECK(typeof(next_attempt_at)='integer' AND next_attempt_at BETWEEN 0 AND 9007199254740991),
 created_at INTEGER NOT NULL CHECK(typeof(created_at)='integer' AND created_at BETWEEN 0 AND 9007199254740991),
 started_at INTEGER CHECK(started_at IS NULL OR (typeof(started_at)='integer' AND started_at BETWEEN 0 AND 9007199254740991)),
 completed_at INTEGER CHECK(completed_at IS NULL OR (typeof(completed_at)='integer' AND completed_at BETWEEN 0 AND 9007199254740991)),
 error_code TEXT CHECK(error_code IS NULL OR error_code IN ('interrupted','storage_error','metadata_not_found','metadata_unavailable','metadata_busy','metadata_rate_limited','invalid_metadata_response','target_changed','metadata_conflict')),
 records_updated INTEGER NOT NULL DEFAULT 0 CHECK(typeof(records_updated)='integer' AND records_updated BETWEEN 0 AND 11001),
 CHECK((name='refresh_series' AND media_type='tv' AND series_id IS NOT NULL AND movie_id IS NULL AND metadata_id IS NULL) OR (name='refresh_movie' AND media_type='movies' AND movie_id IS NOT NULL AND series_id IS NULL AND metadata_id IS NOT NULL)),
 CHECK((status IN ('queued','running','retry_wait') AND completed_at IS NULL) OR (status IN ('succeeded','failed','cancelled') AND completed_at IS NOT NULL)),
 CHECK(status!='queued' OR (attempts=0 AND started_at IS NULL)),
 CHECK(status NOT IN ('running','succeeded','retry_wait') OR (attempts>0 AND started_at IS NOT NULL)),
 CHECK(status!='retry_wait' OR attempts<3),
 CHECK((status IN ('failed','retry_wait') AND error_code IS NOT NULL) OR (status NOT IN ('failed','retry_wait') AND error_code IS NULL)),
 CHECK(status='succeeded' OR records_updated=0)
);
CREATE UNIQUE INDEX metadata_refresh_active_series ON metadata_refresh_commands(series_id) WHERE status IN ('queued','running','retry_wait');
CREATE UNIQUE INDEX metadata_refresh_active_movie ON metadata_refresh_commands(movie_id) WHERE status IN ('queued','running','retry_wait');
CREATE INDEX metadata_refresh_ready ON metadata_refresh_commands(status,next_attempt_at,priority,created_at,id);
CREATE INDEX metadata_refresh_history ON metadata_refresh_commands(created_at,id);
CREATE TRIGGER metadata_refresh_admit BEFORE INSERT ON metadata_refresh_commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands)+(SELECT count(*) FROM metadata_refresh_commands)>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM series s WHERE s.id=NEW.series_id AND s.tvdb_id=NEW.external_id) AND NOT EXISTS(SELECT 1 FROM movies m JOIN movie_metadata d ON d.id=m.metadata_id WHERE m.id=NEW.movie_id AND m.metadata_id=NEW.metadata_id AND d.tmdb_id=NEW.external_id) THEN RAISE(ABORT,'invalid metadata refresh target') END;
END;
CREATE TRIGGER metadata_refresh_transition BEFORE UPDATE ON metadata_refresh_commands
BEGIN
 SELECT CASE WHEN NEW.id IS NOT OLD.id OR NEW.name IS NOT OLD.name OR NEW.media_type IS NOT OLD.media_type OR NEW.series_id IS NOT OLD.series_id OR NEW.movie_id IS NOT OLD.movie_id OR NEW.external_id IS NOT OLD.external_id OR NEW.metadata_id IS NOT OLD.metadata_id OR NEW.created_at IS NOT OLD.created_at OR NEW.priority IS NOT OLD.priority THEN RAISE(ABORT,'command identity is immutable') END;
 SELECT CASE WHEN NOT ((OLD.status IN ('queued','retry_wait') AND NEW.status IN ('running','cancelled','failed')) OR (OLD.status='running' AND NEW.status IN ('running','retry_wait','succeeded','failed','cancelled'))) THEN RAISE(ABORT,'invalid command transition') END;
 SELECT CASE WHEN NEW.attempts != OLD.attempts + CASE WHEN NEW.status='running' AND OLD.status IN ('queued','retry_wait') THEN 1 ELSE 0 END THEN RAISE(ABORT,'invalid command attempt counter') END;
 SELECT CASE WHEN NEW.status IN ('running','succeeded') AND NOT EXISTS(SELECT 1 FROM series s WHERE s.id=NEW.series_id AND s.tvdb_id=NEW.external_id) AND NOT EXISTS(SELECT 1 FROM movies m JOIN movie_metadata d ON d.id=m.metadata_id WHERE m.id=NEW.movie_id AND m.metadata_id=NEW.metadata_id AND d.tmdb_id=NEW.external_id) THEN RAISE(ABORT,'stale metadata refresh target') END;
END;
CREATE TRIGGER metadata_refresh_delete_terminal BEFORE DELETE ON metadata_refresh_commands WHEN OLD.status IN ('queued','running','retry_wait')
BEGIN SELECT RAISE(ABORT,'active commands cannot be deleted'); END;
-- Only admission changes for existing jobs; no existing state or FK is rebuilt.
DROP TRIGGER commands_admit;
CREATE TRIGGER commands_admit BEFORE INSERT ON commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands)+(SELECT count(*) FROM metadata_refresh_commands)>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.provider_id AND p.revision=NEW.provider_revision AND p.enabled=1 AND p.implementation='qbittorrent' AND s.media_type=NEW.media_type) THEN RAISE(ABORT,'invalid refresh provider scope or revision') END;
END;
