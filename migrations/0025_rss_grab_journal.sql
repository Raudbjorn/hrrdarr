-- Concrete RSS decisions and dispatch journal; uncertain submissions never regain POST authority.
CREATE TABLE rss_commands (
 id TEXT PRIMARY KEY NOT NULL CHECK(length(id)=36 AND substr(id,9,1)='-' AND substr(id,14,1)='-' AND substr(id,19,1)='-' AND substr(id,24,1)='-' AND length(replace(id,'-',''))=32 AND replace(id,'-','') NOT GLOB '*[^0-9a-f]*'),
 name TEXT NOT NULL CHECK(name='rss_sync'),
 media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
 priority INTEGER NOT NULL DEFAULT 0 CHECK(typeof(priority)='integer' AND priority IN (0,1)),
 status TEXT NOT NULL DEFAULT 'queued' CHECK(status IN ('queued','running','retry_wait','succeeded','failed','cancelled')),
 attempts INTEGER NOT NULL DEFAULT 0 CHECK(typeof(attempts)='integer' AND attempts BETWEEN 0 AND 3),
 next_attempt_at INTEGER NOT NULL CHECK(typeof(next_attempt_at)='integer' AND next_attempt_at BETWEEN 0 AND 9007199254740991),
 created_at INTEGER NOT NULL CHECK(typeof(created_at)='integer' AND created_at BETWEEN 0 AND 9007199254740991),
 started_at INTEGER CHECK(started_at IS NULL OR (typeof(started_at)='integer' AND started_at BETWEEN 0 AND 9007199254740991)),
 completed_at INTEGER CHECK(completed_at IS NULL OR (typeof(completed_at)='integer' AND completed_at BETWEEN 0 AND 9007199254740991)),
 indexer_id TEXT NOT NULL,
 indexer_revision INTEGER NOT NULL CHECK(typeof(indexer_revision)='integer' AND indexer_revision BETWEEN 1 AND 9007199254740991),
 client_id TEXT NOT NULL,
 client_revision INTEGER NOT NULL CHECK(typeof(client_revision)='integer' AND client_revision BETWEEN 1 AND 9007199254740991),
 error_code TEXT CHECK(error_code IS NULL OR error_code IN ('interrupted','storage_error','provider_changed','provider_unavailable','refresh_timeout','refresh_limit','refresh_failed','key_unavailable','invalid_release','candidate_limit','hash_conflict','submission_unknown','not_observed','unsupported_target','target_conflict','target_changed','preexisting_download','presence_unconfirmed','command_history_full','command_conflict')),
 fetched INTEGER NOT NULL DEFAULT 0 CHECK(typeof(fetched)='integer' AND fetched BETWEEN 0 AND 1000),
 evaluated INTEGER NOT NULL DEFAULT 0 CHECK(typeof(evaluated)='integer' AND evaluated BETWEEN 0 AND 1000),
 rejected INTEGER NOT NULL DEFAULT 0 CHECK(typeof(rejected)='integer' AND rejected BETWEEN 0 AND 1000),
 pending INTEGER NOT NULL DEFAULT 0 CHECK(typeof(pending)='integer' AND pending BETWEEN 0 AND 1000),
 observed INTEGER NOT NULL DEFAULT 0 CHECK(typeof(observed)='integer' AND observed BETWEEN 0 AND 1000),
 uncertain INTEGER NOT NULL DEFAULT 0 CHECK(typeof(uncertain)='integer' AND uncertain BETWEEN 0 AND 1000),
 fetch_complete INTEGER NOT NULL DEFAULT 0 CHECK(typeof(fetch_complete)='integer' AND fetch_complete IN (0,1)),
 CHECK((status IN ('queued','running','retry_wait') AND completed_at IS NULL) OR (status IN ('succeeded','failed','cancelled') AND completed_at IS NOT NULL)),
 CHECK(status!='queued' OR (attempts=0 AND started_at IS NULL)),
 CHECK(status NOT IN ('running','succeeded','retry_wait') OR (attempts>0 AND started_at IS NOT NULL)),
 CHECK(status!='retry_wait' OR attempts<3),
 CHECK((status IN ('failed','retry_wait') AND error_code IS NOT NULL) OR (status NOT IN ('failed','retry_wait') AND error_code IS NULL)),
 CHECK(status!='succeeded' OR fetch_complete=1)
);
CREATE UNIQUE INDEX rss_commands_active ON rss_commands(indexer_id,client_id,media_type) WHERE status IN ('queued','running','retry_wait');
CREATE INDEX rss_commands_ready ON rss_commands(status,next_attempt_at,priority,created_at,id);
CREATE INDEX rss_commands_history ON rss_commands(created_at,id);
CREATE TRIGGER rss_commands_admit BEFORE INSERT ON rss_commands BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands)+(SELECT count(*) FROM metadata_refresh_commands)+(SELECT count(*) FROM blocklist_clear_commands)+(SELECT count(*) FROM rss_commands)>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 OR NEW.fetch_complete!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT (EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.indexer_id AND p.revision=NEW.indexer_revision AND p.implementation IN ('torznab','newznab') AND s.media_type=NEW.media_type AND p.enabled=1) AND EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.client_id AND p.revision=NEW.client_revision AND p.implementation='qbittorrent' AND s.media_type=NEW.media_type AND p.enabled=1)) THEN RAISE(ABORT,'invalid RSS provider scope or revision') END;
END;
CREATE TRIGGER rss_commands_transition BEFORE UPDATE ON rss_commands BEGIN
 SELECT CASE WHEN NEW.id IS NOT OLD.id OR NEW.name IS NOT OLD.name OR NEW.media_type IS NOT OLD.media_type OR NEW.indexer_id IS NOT OLD.indexer_id OR NEW.indexer_revision IS NOT OLD.indexer_revision OR NEW.client_id IS NOT OLD.client_id OR NEW.client_revision IS NOT OLD.client_revision OR NEW.priority IS NOT OLD.priority OR NEW.created_at IS NOT OLD.created_at THEN RAISE(ABORT,'command identity is immutable') END;
 SELECT CASE WHEN NOT ((OLD.status IN ('queued','retry_wait') AND NEW.status IN ('running','cancelled','failed')) OR (OLD.status='running' AND NEW.status IN ('running','retry_wait','succeeded','failed','cancelled'))) THEN RAISE(ABORT,'invalid command transition') END;
 SELECT CASE WHEN NEW.attempts != OLD.attempts + CASE WHEN NEW.status='running' AND OLD.status IN ('queued','retry_wait') THEN 1 ELSE 0 END THEN RAISE(ABORT,'invalid command attempt counter') END;
 SELECT CASE WHEN NEW.fetch_complete<OLD.fetch_complete OR (NEW.fetch_complete>OLD.fetch_complete AND NEW.status!='running') THEN RAISE(ABORT,'invalid RSS fetch checkpoint') END;
END;
CREATE TRIGGER rss_commands_delete_terminal BEFORE DELETE ON rss_commands WHEN OLD.status IN ('queued','running','retry_wait') BEGIN SELECT RAISE(ABORT,'active commands cannot be deleted'); END;
DROP TRIGGER commands_admit;
CREATE TRIGGER commands_admit BEFORE INSERT ON commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands)+(SELECT count(*) FROM metadata_refresh_commands)+(SELECT count(*) FROM blocklist_clear_commands)+(SELECT count(*) FROM rss_commands)>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.provider_id AND p.revision=NEW.provider_revision AND p.enabled=1 AND p.implementation='qbittorrent' AND s.media_type=NEW.media_type) THEN RAISE(ABORT,'invalid refresh provider scope or revision') END;
END;
DROP TRIGGER metadata_refresh_admit;
CREATE TRIGGER metadata_refresh_admit BEFORE INSERT ON metadata_refresh_commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands)+(SELECT count(*) FROM metadata_refresh_commands)+(SELECT count(*) FROM blocklist_clear_commands)+(SELECT count(*) FROM rss_commands)>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM series s WHERE s.id=NEW.series_id AND s.tvdb_id=NEW.external_id) AND NOT EXISTS(SELECT 1 FROM movies m JOIN movie_metadata d ON d.id=m.metadata_id WHERE m.id=NEW.movie_id AND m.metadata_id=NEW.metadata_id AND d.tmdb_id=NEW.external_id) THEN RAISE(ABORT,'invalid metadata refresh target') END;
END;
DROP TRIGGER blocklist_clear_admit;
CREATE TRIGGER blocklist_clear_admit BEFORE INSERT ON blocklist_clear_commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands)+(SELECT count(*) FROM metadata_refresh_commands)+(SELECT count(*) FROM blocklist_clear_commands)+(SELECT count(*) FROM rss_commands)>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
END;
CREATE TABLE rss_schedules (
 id TEXT PRIMARY KEY NOT NULL CHECK(length(id)=36 AND substr(id,9,1)='-' AND substr(id,14,1)='-' AND substr(id,19,1)='-' AND substr(id,24,1)='-' AND length(replace(id,'-',''))=32 AND replace(id,'-','') NOT GLOB '*[^0-9a-f]*'),
 media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
 indexer_id TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
 indexer_revision INTEGER NOT NULL CHECK(typeof(indexer_revision)='integer' AND indexer_revision BETWEEN 1 AND 9007199254740991),
 client_id TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
 client_revision INTEGER NOT NULL CHECK(typeof(client_revision)='integer' AND client_revision BETWEEN 1 AND 9007199254740991),

 revision INTEGER NOT NULL DEFAULT 1 CHECK(typeof(revision)='integer' AND revision BETWEEN 1 AND 9007199254740991),
 interval_seconds INTEGER NOT NULL CHECK(typeof(interval_seconds)='integer' AND interval_seconds BETWEEN 60 AND 86400),
 enabled INTEGER NOT NULL CHECK(typeof(enabled)='integer' AND enabled IN (0,1)),
 next_run_at INTEGER NOT NULL CHECK(typeof(next_run_at)='integer' AND next_run_at BETWEEN 0 AND 9007199254740991),
 last_run_at INTEGER CHECK(last_run_at IS NULL OR (typeof(last_run_at)='integer' AND last_run_at BETWEEN 0 AND 9007199254740991)),
 created_at INTEGER NOT NULL CHECK(typeof(created_at)='integer' AND created_at BETWEEN 0 AND 9007199254740991),
 error_code TEXT CHECK(error_code IS NULL OR error_code IN ('interrupted','storage_error','provider_changed','provider_unavailable','refresh_timeout','refresh_limit','refresh_failed','key_unavailable','invalid_release','candidate_limit','hash_conflict','submission_unknown','not_observed','unsupported_target','target_conflict','target_changed','preexisting_download','presence_unconfirmed','command_history_full','command_conflict')),
 UNIQUE(indexer_id,client_id,media_type),
 CHECK(error_code IS NOT 'provider_changed' OR enabled=0)
);
CREATE INDEX rss_schedules_due ON rss_schedules(enabled,next_run_at);
CREATE TRIGGER rss_schedules_admit BEFORE INSERT ON rss_schedules BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM rss_schedules)>=64 THEN RAISE(ABORT,'RSS schedule capacity reached') END;
 SELECT CASE WHEN NOT (EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.indexer_id AND p.revision=NEW.indexer_revision AND p.implementation IN ('torznab','newznab') AND s.media_type=NEW.media_type AND (NEW.enabled=0 OR p.enabled=1)) AND EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.client_id AND p.revision=NEW.client_revision AND p.implementation='qbittorrent' AND s.media_type=NEW.media_type AND (NEW.enabled=0 OR p.enabled=1))) THEN RAISE(ABORT,'invalid RSS schedule provider scope or revision') END;
END;
CREATE TRIGGER rss_schedules_update BEFORE UPDATE ON rss_schedules BEGIN
 SELECT CASE WHEN NEW.id IS NOT OLD.id OR NEW.media_type IS NOT OLD.media_type OR NEW.indexer_id IS NOT OLD.indexer_id OR NEW.client_id IS NOT OLD.client_id OR NEW.created_at IS NOT OLD.created_at THEN RAISE(ABORT,'RSS schedule identity is immutable') END;
 SELECT CASE WHEN (NEW.enabled=1 OR NEW.indexer_revision IS NOT OLD.indexer_revision OR NEW.client_revision IS NOT OLD.client_revision) AND NOT (EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.indexer_id AND p.revision=NEW.indexer_revision AND p.implementation IN ('torznab','newznab') AND s.media_type=NEW.media_type AND (NEW.enabled=0 OR p.enabled=1)) AND EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.client_id AND p.revision=NEW.client_revision AND p.implementation='qbittorrent' AND s.media_type=NEW.media_type AND (NEW.enabled=0 OR p.enabled=1))) THEN RAISE(ABORT,'invalid RSS schedule provider scope or revision') END;
END;
CREATE TRIGGER rss_provider_changed AFTER UPDATE ON providers BEGIN
 UPDATE rss_schedules SET enabled=0,error_code='provider_changed',revision=min(revision+1,9007199254740991) WHERE indexer_id=OLD.id OR client_id=OLD.id;
END;
CREATE TRIGGER rss_scope_insert AFTER INSERT ON provider_scopes BEGIN
 UPDATE rss_schedules SET enabled=0,error_code='provider_changed',revision=min(revision+1,9007199254740991) WHERE (indexer_id=NEW.provider_id OR client_id=NEW.provider_id) AND media_type=NEW.media_type;
END;
CREATE TRIGGER rss_scope_update AFTER UPDATE ON provider_scopes BEGIN
 UPDATE rss_schedules SET enabled=0,error_code='provider_changed',revision=min(revision+1,9007199254740991) WHERE (indexer_id=NEW.provider_id OR client_id=NEW.provider_id) AND media_type=NEW.media_type;
END;
CREATE TRIGGER rss_scope_delete AFTER DELETE ON provider_scopes BEGIN
 UPDATE rss_schedules SET enabled=0,error_code='provider_changed',revision=min(revision+1,9007199254740991) WHERE (indexer_id=OLD.provider_id OR client_id=OLD.provider_id) AND media_type=OLD.media_type;
END;
CREATE TABLE rss_candidates (
 id TEXT PRIMARY KEY NOT NULL CHECK(length(id)=36 AND substr(id,9,1)='-' AND substr(id,14,1)='-' AND substr(id,19,1)='-' AND substr(id,24,1)='-' AND length(replace(id,'-',''))=32 AND replace(id,'-','') NOT GLOB '*[^0-9a-f]*'),
 command_id TEXT REFERENCES rss_commands(id) ON DELETE SET NULL,
 media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
 indexer_id TEXT NOT NULL,
 indexer_revision INTEGER NOT NULL CHECK(typeof(indexer_revision)='integer' AND indexer_revision BETWEEN 1 AND 9007199254740991),
 client_id TEXT NOT NULL,
 client_revision INTEGER NOT NULL CHECK(typeof(client_revision)='integer' AND client_revision BETWEEN 1 AND 9007199254740991),

 fingerprint TEXT NOT NULL CHECK(length(fingerprint)=64 AND fingerprint NOT GLOB '*[^0-9a-f]*'),
 title TEXT NOT NULL CHECK(typeof(title)='text' AND length(CAST(title AS BLOB)) BETWEEN 1 AND 1024 AND instr(title,char(0))=0),
 private_payload BLOB CHECK(private_payload IS NULL OR (typeof(private_payload)='blob' AND length(private_payload) BETWEEN 29 AND 65565)),
 series_id INTEGER REFERENCES series(id) ON DELETE RESTRICT,
 movie_id INTEGER REFERENCES movies(id) ON DELETE RESTRICT,
 status TEXT NOT NULL CHECK(status IN ('pending','rejected','prepared','submitting','reconciling','observed','needs_attention','cancelled')),
 decision_reasons_json TEXT NOT NULL CHECK(length(CAST(decision_reasons_json AS BLOB))<=8192 AND json_valid(decision_reasons_json) AND json_type(decision_reasons_json)='array' AND json_array_length(decision_reasons_json)<=64),
 not_before INTEGER CHECK(not_before IS NULL OR (typeof(not_before)='integer' AND not_before BETWEEN 0 AND 9007199254740991)),
 submission_identity_json TEXT CHECK(submission_identity_json IS NULL OR (length(CAST(submission_identity_json AS BLOB))<=4096 AND json_valid(submission_identity_json) AND json_type(submission_identity_json)='object')),
 observed_hash TEXT CHECK(observed_hash IS NULL OR (typeof(observed_hash)='text' AND length(observed_hash) IN (40,64) AND observed_hash NOT GLOB '*[^0-9a-f]*')),
 attempts INTEGER NOT NULL DEFAULT 0 CHECK(typeof(attempts)='integer' AND attempts BETWEEN 0 AND 3),
 created_at INTEGER NOT NULL CHECK(typeof(created_at)='integer' AND created_at BETWEEN 0 AND 9007199254740991),
 updated_at INTEGER NOT NULL CHECK(typeof(updated_at)='integer' AND updated_at BETWEEN 0 AND 9007199254740991),
 error_code TEXT CHECK(error_code IS NULL OR error_code IN ('interrupted','storage_error','provider_changed','provider_unavailable','refresh_timeout','refresh_limit','refresh_failed','key_unavailable','invalid_release','candidate_limit','hash_conflict','submission_unknown','not_observed','unsupported_target','target_conflict','target_changed','preexisting_download','presence_unconfirmed','command_history_full','command_conflict')),
 UNIQUE(indexer_id,indexer_revision,client_id,client_revision,media_type,fingerprint),
 UNIQUE(id,series_id),
 CHECK((media_type='tv' AND movie_id IS NULL) OR (media_type='movies' AND series_id IS NULL)),
 CHECK(status NOT IN ('pending','prepared') OR private_payload IS NOT NULL),
 CHECK(status NOT IN ('prepared','submitting','reconciling','observed','needs_attention') OR (submission_identity_json IS NOT NULL AND (series_id IS NOT NULL OR movie_id IS NOT NULL))),
 CHECK(status NOT IN ('submitting','reconciling','observed','needs_attention') OR private_payload IS NULL),
 CHECK((status='observed')=(observed_hash IS NOT NULL)),
 CHECK(status!='reconciling' OR attempts>0)
);
CREATE UNIQUE INDEX rss_observed_remote_owned ON rss_candidates(client_id,observed_hash) WHERE observed_hash IS NOT NULL;
CREATE INDEX rss_candidates_due ON rss_candidates(status,not_before,created_at,id);
CREATE INDEX rss_candidates_command ON rss_candidates(command_id,status);
CREATE TABLE rss_candidate_episodes (
 candidate_id TEXT NOT NULL,
 series_id INTEGER NOT NULL,
 episode_id INTEGER NOT NULL,
 PRIMARY KEY(candidate_id,episode_id),
 FOREIGN KEY(candidate_id,series_id) REFERENCES rss_candidates(id,series_id) ON DELETE CASCADE,
 FOREIGN KEY(episode_id,series_id) REFERENCES episodes(id,series_id) ON DELETE RESTRICT
);
CREATE TABLE rss_hash_claims (
 client_id TEXT NOT NULL,
 hash TEXT NOT NULL CHECK(length(hash) IN (40,64) AND hash NOT GLOB '*[^0-9a-f]*'),
 candidate_id TEXT NOT NULL REFERENCES rss_candidates(id) ON DELETE RESTRICT,
 PRIMARY KEY(client_id,hash)
);
CREATE TRIGGER rss_candidate_admit BEFORE INSERT ON rss_candidates BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM rss_candidates)>=1024 THEN RAISE(ABORT,'RSS candidate capacity reached') END;
 SELECT CASE WHEN (SELECT COALESCE(sum(length(private_payload)),0) FROM rss_candidates)+COALESCE(length(NEW.private_payload),0)>16777216 THEN RAISE(ABORT,'RSS payload capacity reached') END;
 SELECT CASE WHEN NEW.status NOT IN ('pending','rejected') OR NEW.attempts!=0 OR NEW.submission_identity_json IS NOT NULL THEN RAISE(ABORT,'invalid initial RSS candidate state') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM rss_commands c WHERE c.id=NEW.command_id AND c.media_type=NEW.media_type AND c.indexer_id=NEW.indexer_id AND c.indexer_revision=NEW.indexer_revision AND c.client_id=NEW.client_id AND c.client_revision=NEW.client_revision) THEN RAISE(ABORT,'candidate requires matching RSS command') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM json_each(NEW.decision_reasons_json) WHERE type!='text' OR length(value)>128 OR value GLOB '*[^a-z0-9_]*') THEN RAISE(ABORT,'invalid decision reason') END;
END;
CREATE TRIGGER rss_candidate_transition BEFORE UPDATE ON rss_candidates BEGIN
 SELECT CASE WHEN NEW.id IS NOT OLD.id OR NEW.media_type IS NOT OLD.media_type OR NEW.indexer_id IS NOT OLD.indexer_id OR NEW.indexer_revision IS NOT OLD.indexer_revision OR NEW.client_id IS NOT OLD.client_id OR NEW.client_revision IS NOT OLD.client_revision OR NEW.fingerprint IS NOT OLD.fingerprint OR NEW.title IS NOT OLD.title OR NEW.created_at IS NOT OLD.created_at OR (NEW.command_id IS NOT OLD.command_id AND NEW.command_id IS NOT NULL) THEN RAISE(ABORT,'candidate identity is immutable') END;
 SELECT CASE WHEN NOT ((NEW.command_id IS NULL AND OLD.command_id IS NOT NULL AND NEW.id IS OLD.id AND NEW.media_type IS OLD.media_type AND NEW.indexer_id IS OLD.indexer_id AND NEW.indexer_revision IS OLD.indexer_revision AND NEW.client_id IS OLD.client_id AND NEW.client_revision IS OLD.client_revision AND NEW.fingerprint IS OLD.fingerprint AND NEW.title IS OLD.title AND NEW.created_at IS OLD.created_at AND NEW.private_payload IS OLD.private_payload AND NEW.series_id IS OLD.series_id AND NEW.movie_id IS OLD.movie_id AND NEW.status IS OLD.status AND NEW.decision_reasons_json IS OLD.decision_reasons_json AND NEW.not_before IS OLD.not_before AND NEW.submission_identity_json IS OLD.submission_identity_json AND NEW.observed_hash IS OLD.observed_hash AND NEW.attempts IS OLD.attempts AND NEW.updated_at IS OLD.updated_at AND NEW.error_code IS OLD.error_code) OR (OLD.status='pending' AND NEW.status IN ('pending','rejected','prepared','cancelled')) OR (OLD.status='prepared' AND NEW.status IN ('prepared','submitting','rejected','cancelled')) OR (OLD.status='submitting' AND NEW.status IN ('reconciling','observed','needs_attention')) OR (OLD.status='reconciling' AND NEW.status IN ('reconciling','needs_attention'))) THEN RAISE(ABORT,'invalid RSS candidate transition') END;
 SELECT CASE WHEN OLD.status!='pending' AND (NEW.series_id IS NOT OLD.series_id OR NEW.movie_id IS NOT OLD.movie_id OR NEW.submission_identity_json IS NOT OLD.submission_identity_json) THEN RAISE(ABORT,'prepared RSS target is immutable') END;
 SELECT CASE WHEN NEW.private_payload IS NOT OLD.private_payload AND NOT (NEW.private_payload IS NULL AND ((OLD.status='prepared' AND NEW.status='submitting') OR NEW.status IN ('rejected','cancelled'))) THEN RAISE(ABORT,'RSS payload is immutable until dispatch') END;
 SELECT CASE WHEN NEW.attempts!=OLD.attempts+CASE WHEN NEW.status='reconciling' AND OLD.status IN ('submitting','reconciling') THEN 1 ELSE 0 END THEN RAISE(ABORT,'invalid reconciliation attempt counter') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM json_each(NEW.decision_reasons_json) WHERE type!='text' OR length(value)>128 OR value GLOB '*[^a-z0-9_]*') THEN RAISE(ABORT,'invalid decision reason') END;
 SELECT CASE WHEN NEW.status='prepared' AND ((NEW.media_type='tv' AND (SELECT count(*) FROM rss_candidate_episodes WHERE candidate_id=NEW.id)!=1) OR (NEW.media_type='movies' AND EXISTS(SELECT 1 FROM rss_candidate_episodes WHERE candidate_id=NEW.id))) THEN RAISE(ABORT,'unsupported dispatch target membership') END;
 SELECT CASE WHEN NEW.status='prepared' AND (json_extract(NEW.submission_identity_json,'$.version') IS NOT 1 OR json_extract(NEW.submission_identity_json,'$.target.media_type') IS NOT CASE WHEN NEW.media_type='tv' THEN 'episode' ELSE 'movie' END OR json_extract(NEW.submission_identity_json,'$.target.id') IS NOT CASE WHEN NEW.media_type='tv' THEN (SELECT episode_id FROM rss_candidate_episodes WHERE candidate_id=NEW.id) ELSE NEW.movie_id END OR json_type(NEW.submission_identity_json,'$.hashes') IS NOT 'array' OR json_array_length(NEW.submission_identity_json,'$.hashes') NOT BETWEEN 1 AND 2 OR EXISTS(SELECT 1 FROM json_each(NEW.submission_identity_json,'$.hashes') WHERE type!='text' OR length(value) NOT IN (40,64) OR value GLOB '*[^0-9a-f]*') OR (SELECT count(DISTINCT value) FROM json_each(NEW.submission_identity_json,'$.hashes'))!=json_array_length(NEW.submission_identity_json,'$.hashes')) THEN RAISE(ABORT,'invalid submission identity') END;
 SELECT CASE WHEN NEW.status='submitting' AND EXISTS(SELECT 1 FROM json_each(NEW.submission_identity_json,'$.hashes') j WHERE NOT EXISTS(SELECT 1 FROM rss_hash_claims h WHERE h.client_id=NEW.client_id AND h.hash=j.value AND h.candidate_id=NEW.id)) THEN RAISE(ABORT,'dispatch requires complete hash ownership') END;
END;
CREATE TRIGGER rss_candidate_delete BEFORE DELETE ON rss_candidates WHEN OLD.status NOT IN ('rejected','cancelled') BEGIN SELECT RAISE(ABORT,'only unclaimed rejected or cancelled candidates may be removed'); END;
CREATE TRIGGER rss_episode_insert BEFORE INSERT ON rss_candidate_episodes BEGIN
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM rss_candidates WHERE id=NEW.candidate_id AND status='pending' AND media_type='tv') OR (SELECT count(*) FROM rss_candidate_episodes WHERE candidate_id=NEW.candidate_id)>=1 THEN RAISE(ABORT,'invalid candidate episode membership') END;
END;
CREATE TRIGGER rss_episode_update BEFORE UPDATE ON rss_candidate_episodes BEGIN SELECT RAISE(ABORT,'replace pending membership explicitly'); END;
CREATE TRIGGER rss_episode_delete BEFORE DELETE ON rss_candidate_episodes WHEN EXISTS(SELECT 1 FROM rss_candidates WHERE id=OLD.candidate_id AND status!='pending') BEGIN SELECT RAISE(ABORT,'prepared membership is immutable'); END;
CREATE TRIGGER rss_hash_admit BEFORE INSERT ON rss_hash_claims
WHEN NOT EXISTS(SELECT 1 FROM rss_candidates c,json_each(c.submission_identity_json,'$.hashes') j WHERE c.id=NEW.candidate_id AND c.client_id=NEW.client_id AND c.status='prepared' AND j.value=NEW.hash)
BEGIN SELECT RAISE(ABORT,'invalid hash ownership claim'); END;
CREATE TRIGGER rss_hash_immutable BEFORE UPDATE ON rss_hash_claims BEGIN SELECT RAISE(ABORT,'hash ownership is immutable'); END;
CREATE TRIGGER rss_hash_retained BEFORE DELETE ON rss_hash_claims BEGIN SELECT RAISE(ABORT,'hash ownership must be retained'); END;

-- A target remains owned after observation/uncertainty until explicit future resolution.
CREATE UNIQUE INDEX rss_movie_target_owned ON rss_candidates(movie_id) WHERE status IN ('prepared','submitting','reconciling','observed','needs_attention');
CREATE TRIGGER rss_episode_target_owned BEFORE UPDATE OF status ON rss_candidates
WHEN NEW.status='prepared' AND EXISTS(
 SELECT 1 FROM rss_candidate_episodes incoming JOIN rss_candidate_episodes existing ON existing.episode_id=incoming.episode_id
 JOIN rss_candidates owner ON owner.id=existing.candidate_id
 WHERE incoming.candidate_id=NEW.id AND owner.id!=NEW.id AND owner.status IN ('prepared','submitting','reconciling','observed','needs_attention'))
BEGIN SELECT RAISE(ABORT,'episode target already owned'); END;
CREATE TRIGGER rss_identity_closed BEFORE UPDATE OF status,submission_identity_json ON rss_candidates
WHEN NEW.status='prepared' AND (
 (SELECT count(*) FROM json_each(NEW.submission_identity_json))!=5
 OR EXISTS(SELECT 1 FROM json_each(NEW.submission_identity_json) WHERE key NOT IN ('version','target','hashes','settings_fingerprint','payload_sha256'))
 OR json_type(NEW.submission_identity_json,'$.version') IS NOT 'integer'
 OR json_type(NEW.submission_identity_json,'$.target') IS NOT 'object'
 OR (SELECT count(*) FROM json_each(NEW.submission_identity_json,'$.target'))!=2
 OR EXISTS(SELECT 1 FROM json_each(NEW.submission_identity_json,'$.target') WHERE key NOT IN ('media_type','id'))
 OR json_type(NEW.submission_identity_json,'$.target.id') IS NOT 'integer'
 OR json_type(NEW.submission_identity_json,'$.settings_fingerprint') IS NOT 'text'
 OR length(json_extract(NEW.submission_identity_json,'$.settings_fingerprint'))!=64
 OR json_extract(NEW.submission_identity_json,'$.settings_fingerprint') GLOB '*[^0-9a-f]*'
 OR json_type(NEW.submission_identity_json,'$.payload_sha256') IS NOT 'text'
 OR length(json_extract(NEW.submission_identity_json,'$.payload_sha256'))!=64
 OR json_extract(NEW.submission_identity_json,'$.payload_sha256') GLOB '*[^0-9a-f]*'
 OR (SELECT count(DISTINCT length(value)) FROM json_each(NEW.submission_identity_json,'$.hashes'))!=json_array_length(NEW.submission_identity_json,'$.hashes'))
BEGIN SELECT RAISE(ABORT,'submission identity must contain only validated receipt facts'); END;
