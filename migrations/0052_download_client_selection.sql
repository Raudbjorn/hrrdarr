-- Client selection intent and bounded per-client dispatch evidence.
-- The owning migration connection disables foreign keys before its transaction;
-- every row and relationship is checked before commit and application connections keep enforcement on.

CREATE TEMP TABLE _selection52_rss_commands AS SELECT * FROM rss_commands;

DROP TABLE rss_commands;

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
 client_id TEXT,
 client_revision INTEGER CHECK(client_revision IS NULL OR (typeof(client_revision)='integer' AND client_revision BETWEEN 1 AND 9007199254740991)),
 error_code TEXT CHECK(error_code IS NULL OR error_code IN ('interrupted','storage_error','provider_changed','provider_unavailable','refresh_timeout','refresh_limit','refresh_failed','key_unavailable','invalid_release','candidate_limit','hash_conflict','submission_unknown','not_observed','unsupported_target','target_conflict','target_changed','preexisting_download','presence_unconfirmed','command_history_full','command_conflict','download_client_unavailable','payload_expired','selection_limit','release_changed','client_selection_exhausted')),
 fetched INTEGER NOT NULL DEFAULT 0 CHECK(typeof(fetched)='integer' AND fetched BETWEEN 0 AND 1000),
 evaluated INTEGER NOT NULL DEFAULT 0 CHECK(typeof(evaluated)='integer' AND evaluated BETWEEN 0 AND 1000),
 rejected INTEGER NOT NULL DEFAULT 0 CHECK(typeof(rejected)='integer' AND rejected BETWEEN 0 AND 1000),
 pending INTEGER NOT NULL DEFAULT 0 CHECK(typeof(pending)='integer' AND pending BETWEEN 0 AND 1000),
 observed INTEGER NOT NULL DEFAULT 0 CHECK(typeof(observed)='integer' AND observed BETWEEN 0 AND 1000),
 uncertain INTEGER NOT NULL DEFAULT 0 CHECK(typeof(uncertain)='integer' AND uncertain BETWEEN 0 AND 1000),
 fetch_complete INTEGER NOT NULL DEFAULT 0 CHECK(typeof(fetch_complete)='integer' AND fetch_complete IN (0,1)),
 selection_kind TEXT NOT NULL DEFAULT 'legacy_fixed' CHECK(selection_kind IN ('legacy_fixed','default','explicit')),
 CHECK((status IN ('queued','running','retry_wait') AND completed_at IS NULL) OR (status IN ('succeeded','failed','cancelled') AND completed_at IS NOT NULL)),
 CHECK(status!='queued' OR (attempts=0 AND started_at IS NULL)),
 CHECK(status NOT IN ('running','succeeded','retry_wait') OR (attempts>0 AND started_at IS NOT NULL)),
 CHECK(status!='retry_wait' OR attempts<3),
 CHECK((status IN ('failed','retry_wait') AND error_code IS NOT NULL) OR (status NOT IN ('failed','retry_wait') AND error_code IS NULL)),
 CHECK(status!='succeeded' OR fetch_complete=1),
 CHECK((client_id IS NULL)=(client_revision IS NULL)),
 CHECK((selection_kind='default' AND client_id IS NULL) OR (selection_kind!='default' AND client_id IS NOT NULL))
);

INSERT INTO rss_commands(id,name,media_type,priority,status,attempts,next_attempt_at,created_at,started_at,completed_at,indexer_id,indexer_revision,client_id,client_revision,error_code,fetched,evaluated,rejected,pending,observed,uncertain,fetch_complete) SELECT id,name,media_type,priority,status,attempts,next_attempt_at,created_at,started_at,completed_at,indexer_id,indexer_revision,client_id,client_revision,error_code,fetched,evaluated,rejected,pending,observed,uncertain,fetch_complete FROM _selection52_rss_commands;

DROP TABLE _selection52_rss_commands;

CREATE TEMP TABLE _selection52_search_commands AS SELECT * FROM search_commands;

DROP TABLE search_commands;

CREATE TABLE search_commands (
 id TEXT PRIMARY KEY NOT NULL CHECK(length(id)=36 AND substr(id,9,1)='-' AND substr(id,14,1)='-' AND substr(id,19,1)='-' AND substr(id,24,1)='-' AND length(replace(id,'-',''))=32 AND replace(id,'-','') NOT GLOB '*[^0-9a-f]*'),
 mode TEXT NOT NULL CHECK(mode IN ('automatic','interactive')),
 decision_context TEXT NOT NULL DEFAULT 'user_search' CHECK(decision_context='user_search'),
 requested_episode_id INTEGER CHECK(requested_episode_id IS NULL OR (typeof(requested_episode_id)='integer' AND requested_episode_id BETWEEN 1 AND 9007199254740991)),
 requested_movie_id INTEGER CHECK(requested_movie_id IS NULL OR (typeof(requested_movie_id)='integer' AND requested_movie_id BETWEEN 1 AND 9007199254740991)),
 captured_target_json TEXT NOT NULL CHECK(length(CAST(captured_target_json AS BLOB))<=8192 AND json_valid(captured_target_json) AND json_type(captured_target_json)='object'),
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
 client_id TEXT,
 client_revision INTEGER CHECK(client_revision IS NULL OR (typeof(client_revision)='integer' AND client_revision BETWEEN 1 AND 9007199254740991)),
 error_code TEXT CHECK(error_code IS NULL OR error_code IN ('interrupted','storage_error','provider_changed','provider_unavailable','refresh_timeout','refresh_limit','refresh_failed','key_unavailable','invalid_release','candidate_limit','hash_conflict','submission_unknown','not_observed','unsupported_target','target_conflict','target_changed','preexisting_download','presence_unconfirmed','command_history_full','command_conflict','search_limit','no_eligible_release','result_expired','invalid_search_target')),
 fetched INTEGER NOT NULL DEFAULT 0 CHECK(typeof(fetched)='integer' AND fetched BETWEEN 0 AND 1000),
 fetch_complete INTEGER NOT NULL DEFAULT 0 CHECK(typeof(fetch_complete)='integer' AND fetch_complete IN (0,1)),
 selection_kind TEXT NOT NULL DEFAULT 'legacy_fixed' CHECK(selection_kind IN ('legacy_fixed','default','explicit')),
 CHECK((status IN ('queued','running','retry_wait') AND completed_at IS NULL) OR (status IN ('succeeded','failed','cancelled') AND completed_at IS NOT NULL)),
 CHECK(status!='queued' OR (attempts=0 AND started_at IS NULL)),
 CHECK(status NOT IN ('running','succeeded','retry_wait') OR (attempts>0 AND started_at IS NOT NULL)),
 CHECK(status!='retry_wait' OR attempts<3),
 CHECK((status IN ('failed','retry_wait') AND error_code IS NOT NULL) OR (status NOT IN ('failed','retry_wait') AND error_code IS NULL)),
 CHECK(status!='succeeded' OR fetch_complete=1),
 CHECK((media_type='tv' AND requested_episode_id IS NOT NULL AND requested_movie_id IS NULL) OR (media_type='movies' AND requested_movie_id IS NOT NULL AND requested_episode_id IS NULL)),
 CHECK((client_id IS NULL)=(client_revision IS NULL)),
 CHECK((selection_kind='default' AND client_id IS NULL) OR (selection_kind!='default' AND client_id IS NOT NULL))
);

INSERT INTO search_commands(id,mode,decision_context,requested_episode_id,requested_movie_id,captured_target_json,media_type,priority,status,attempts,next_attempt_at,created_at,started_at,completed_at,indexer_id,indexer_revision,client_id,client_revision,error_code,fetched,fetch_complete) SELECT id,mode,decision_context,requested_episode_id,requested_movie_id,captured_target_json,media_type,priority,status,attempts,next_attempt_at,created_at,started_at,completed_at,indexer_id,indexer_revision,client_id,client_revision,error_code,fetched,fetch_complete FROM _selection52_search_commands;

DROP TABLE _selection52_search_commands;

CREATE TEMP TABLE _selection52_rss_schedules AS SELECT * FROM rss_schedules;

DROP TABLE rss_schedules;

CREATE TABLE rss_schedules (
 id TEXT PRIMARY KEY NOT NULL CHECK(length(id)=36 AND substr(id,9,1)='-' AND substr(id,14,1)='-' AND substr(id,19,1)='-' AND substr(id,24,1)='-' AND length(replace(id,'-',''))=32 AND replace(id,'-','') NOT GLOB '*[^0-9a-f]*'),
 media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
 indexer_id TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
 indexer_revision INTEGER NOT NULL CHECK(typeof(indexer_revision)='integer' AND indexer_revision BETWEEN 1 AND 9007199254740991),
 client_id TEXT REFERENCES providers(id) ON DELETE CASCADE,
 client_revision INTEGER CHECK(client_revision IS NULL OR (typeof(client_revision)='integer' AND client_revision BETWEEN 1 AND 9007199254740991)),
 revision INTEGER NOT NULL DEFAULT 1 CHECK(typeof(revision)='integer' AND revision BETWEEN 1 AND 9007199254740991),
 interval_seconds INTEGER NOT NULL CHECK(typeof(interval_seconds)='integer' AND interval_seconds BETWEEN 60 AND 86400),
 enabled INTEGER NOT NULL CHECK(typeof(enabled)='integer' AND enabled IN (0,1)),
 next_run_at INTEGER NOT NULL CHECK(typeof(next_run_at)='integer' AND next_run_at BETWEEN 0 AND 9007199254740991),
 last_run_at INTEGER CHECK(last_run_at IS NULL OR (typeof(last_run_at)='integer' AND last_run_at BETWEEN 0 AND 9007199254740991)),
 created_at INTEGER NOT NULL CHECK(typeof(created_at)='integer' AND created_at BETWEEN 0 AND 9007199254740991),
 error_code TEXT CHECK(error_code IS NULL OR error_code IN ('interrupted','storage_error','provider_changed','provider_unavailable','refresh_timeout','refresh_limit','refresh_failed','key_unavailable','invalid_release','candidate_limit','hash_conflict','submission_unknown','not_observed','unsupported_target','target_conflict','target_changed','preexisting_download','presence_unconfirmed','command_history_full','command_conflict','download_client_unavailable','payload_expired','selection_limit','release_changed','client_selection_exhausted')),
 selection_kind TEXT NOT NULL DEFAULT 'legacy_fixed' CHECK(selection_kind IN ('legacy_fixed','default','explicit')),
 CHECK(error_code IS NOT 'provider_changed' OR enabled=0),
 CHECK((client_id IS NULL)=(client_revision IS NULL)),
 CHECK((selection_kind='default' AND client_id IS NULL) OR (selection_kind!='default' AND client_id IS NOT NULL))
);

INSERT INTO rss_schedules(id,media_type,indexer_id,indexer_revision,client_id,client_revision,revision,interval_seconds,enabled,next_run_at,last_run_at,created_at,error_code) SELECT id,media_type,indexer_id,indexer_revision,client_id,client_revision,revision,interval_seconds,enabled,next_run_at,last_run_at,created_at,error_code FROM _selection52_rss_schedules;

DROP TABLE _selection52_rss_schedules;

CREATE TEMP TABLE _selection52_rss_candidates AS SELECT * FROM rss_candidates;

DROP TABLE rss_candidates;

CREATE TABLE rss_candidates (
 id TEXT PRIMARY KEY NOT NULL CHECK(length(id)=36 AND substr(id,9,1)='-' AND substr(id,14,1)='-' AND substr(id,19,1)='-' AND substr(id,24,1)='-' AND length(replace(id,'-',''))=32 AND replace(id,'-','') NOT GLOB '*[^0-9a-f]*'),
 command_id TEXT REFERENCES rss_commands(id) ON DELETE SET NULL,
 media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
 indexer_id TEXT NOT NULL,
 indexer_revision INTEGER NOT NULL CHECK(typeof(indexer_revision)='integer' AND indexer_revision BETWEEN 1 AND 9007199254740991),
 client_id TEXT,
 client_revision INTEGER CHECK(client_revision IS NULL OR (typeof(client_revision)='integer' AND client_revision BETWEEN 1 AND 9007199254740991)),
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
 error_code TEXT CHECK(error_code IS NULL OR error_code IN ('interrupted','storage_error','provider_changed','provider_unavailable','refresh_timeout','refresh_limit','refresh_failed','key_unavailable','invalid_release','candidate_limit','hash_conflict','submission_unknown','not_observed','unsupported_target','target_conflict','target_changed','preexisting_download','presence_unconfirmed','command_history_full','command_conflict','download_client_unavailable','payload_expired','selection_limit','release_changed','client_selection_exhausted','local_storage_retry')),
 search_result_id TEXT REFERENCES search_results(id) ON DELETE RESTRICT,
 comparison_facts_json TEXT
 CHECK(comparison_facts_json IS NULL OR (
  typeof(comparison_facts_json)='text' AND length(CAST(comparison_facts_json AS BLOB))<=16384
  AND json_valid(comparison_facts_json) AND json_type(comparison_facts_json)='object'
  AND json_type(comparison_facts_json,'$.version') IS 'integer'
  AND json_extract(comparison_facts_json,'$.version')=1
 )),
 selection_kind TEXT NOT NULL DEFAULT 'legacy_fixed' CHECK(selection_kind IN ('legacy_fixed','default','explicit')),
 selection_round INTEGER NOT NULL DEFAULT 0 CHECK(typeof(selection_round)='integer' AND selection_round BETWEEN 0 AND 128),
 selection_plan_json TEXT CHECK(selection_plan_json IS NULL OR (typeof(selection_plan_json)='text' AND length(CAST(selection_plan_json AS BLOB))<=16384 AND json_valid(selection_plan_json) AND json_type(selection_plan_json)='array' AND json_array_length(selection_plan_json) BETWEEN 1 AND 128)),
 active_attempt INTEGER CHECK(active_attempt IS NULL OR (typeof(active_attempt)='integer' AND active_attempt BETWEEN 1 AND 128)),
 selection_strict INTEGER NOT NULL DEFAULT 0 CHECK(typeof(selection_strict)='integer' AND selection_strict IN (0,1)),
 payload_expires_at INTEGER CHECK(payload_expires_at IS NULL OR (typeof(payload_expires_at)='integer' AND payload_expires_at BETWEEN 1800 AND 9007199254740991)),
 UNIQUE(id,series_id),
 CHECK((media_type='tv' AND movie_id IS NULL) OR (media_type='movies' AND series_id IS NULL)),
 CHECK(status NOT IN ('pending','prepared') OR private_payload IS NOT NULL),
 CHECK(status NOT IN ('prepared','submitting','reconciling','observed','needs_attention') OR (submission_identity_json IS NOT NULL AND (series_id IS NOT NULL OR movie_id IS NOT NULL))),
 CHECK(status NOT IN ('submitting','reconciling','observed','needs_attention') OR private_payload IS NULL OR (selection_kind!='legacy_fixed' AND status='submitting' AND payload_expires_at IS NOT NULL)),
 CHECK((status='observed')=(observed_hash IS NOT NULL)),
 CHECK(status!='reconciling' OR attempts>0),
 CHECK((client_id IS NULL)=(client_revision IS NULL)),
 CHECK(selection_kind='default' OR client_id IS NOT NULL),
 CHECK(status NOT IN ('prepared','submitting','reconciling','observed','needs_attention') OR client_id IS NOT NULL),
 CHECK(selection_kind!='legacy_fixed' OR (selection_round=0 AND selection_plan_json IS NULL AND active_attempt IS NULL AND selection_strict=0 AND payload_expires_at IS NULL)),
 CHECK((selection_round=0)=(payload_expires_at IS NULL)),
 CHECK(selection_plan_json IS NULL OR selection_round>0),
 CHECK(active_attempt IS NULL OR selection_plan_json IS NOT NULL),
 CHECK(selection_kind='legacy_fixed' OR status NOT IN ('rejected','cancelled','reconciling','observed','needs_attention') OR private_payload IS NULL),
 FOREIGN KEY(id,active_attempt) REFERENCES rss_client_attempts(candidate_id,ordinal) DEFERRABLE INITIALLY DEFERRED
);

INSERT INTO rss_candidates(id,command_id,media_type,indexer_id,indexer_revision,client_id,client_revision,fingerprint,title,private_payload,series_id,movie_id,status,decision_reasons_json,not_before,submission_identity_json,observed_hash,attempts,created_at,updated_at,error_code,search_result_id,comparison_facts_json) SELECT id,command_id,media_type,indexer_id,indexer_revision,client_id,client_revision,fingerprint,title,private_payload,series_id,movie_id,status,decision_reasons_json,not_before,submission_identity_json,observed_hash,attempts,created_at,updated_at,error_code,search_result_id,comparison_facts_json FROM _selection52_rss_candidates;

DROP TABLE _selection52_rss_candidates;

CREATE UNIQUE INDEX rss_commands_active ON rss_commands(indexer_id,selection_kind,coalesce(client_id,''),media_type) WHERE status IN ('queued','running','retry_wait');

CREATE INDEX rss_commands_ready ON rss_commands(status,next_attempt_at,priority,created_at,id);

CREATE INDEX rss_commands_history ON rss_commands(created_at,id);

CREATE TRIGGER rss_commands_delete_terminal BEFORE DELETE ON rss_commands WHEN OLD.status IN ('queued','running','retry_wait') BEGIN SELECT RAISE(ABORT,'active commands cannot be deleted'); END;

CREATE INDEX rss_schedules_due ON rss_schedules(enabled,next_run_at);

CREATE TRIGGER rss_schedules_admit BEFORE INSERT ON rss_schedules BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM rss_schedules)>=64 THEN RAISE(ABORT,'RSS schedule capacity reached') END;
 SELECT CASE WHEN NOT (EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.indexer_id AND p.revision=NEW.indexer_revision AND p.implementation IN ('torznab','newznab') AND s.media_type=NEW.media_type AND (NEW.enabled=0 OR p.enabled=1)) AND (NEW.selection_kind='default' OR EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.client_id AND p.revision=NEW.client_revision AND p.implementation='qbittorrent' AND s.media_type=NEW.media_type AND (NEW.enabled=0 OR p.enabled=1)))) THEN RAISE(ABORT,'invalid RSS schedule provider scope or revision') END;
END;

CREATE TRIGGER rss_schedules_update BEFORE UPDATE ON rss_schedules BEGIN
 SELECT CASE WHEN NEW.selection_kind IS NOT OLD.selection_kind OR NEW.id IS NOT OLD.id OR NEW.media_type IS NOT OLD.media_type OR NEW.indexer_id IS NOT OLD.indexer_id OR NEW.client_id IS NOT OLD.client_id OR NEW.created_at IS NOT OLD.created_at THEN RAISE(ABORT,'RSS schedule identity is immutable') END;
 SELECT CASE WHEN (NEW.enabled=1 OR NEW.indexer_revision IS NOT OLD.indexer_revision OR NEW.client_revision IS NOT OLD.client_revision) AND NOT (EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.indexer_id AND p.revision=NEW.indexer_revision AND p.implementation IN ('torznab','newznab') AND s.media_type=NEW.media_type AND (NEW.enabled=0 OR p.enabled=1)) AND (NEW.selection_kind='default' OR EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.client_id AND p.revision=NEW.client_revision AND p.implementation='qbittorrent' AND s.media_type=NEW.media_type AND (NEW.enabled=0 OR p.enabled=1)))) THEN RAISE(ABORT,'invalid RSS schedule provider scope or revision') END;
END;

CREATE UNIQUE INDEX rss_observed_remote_owned ON rss_candidates(client_id,observed_hash) WHERE observed_hash IS NOT NULL;

CREATE INDEX rss_candidates_due ON rss_candidates(status,not_before,created_at,id);

CREATE INDEX rss_candidates_command ON rss_candidates(command_id,status);

CREATE TRIGGER rss_candidate_transition BEFORE UPDATE ON rss_candidates BEGIN
 SELECT CASE WHEN NEW.selection_kind IS NOT OLD.selection_kind OR NEW.id IS NOT OLD.id OR NEW.media_type IS NOT OLD.media_type OR NEW.indexer_id IS NOT OLD.indexer_id OR NEW.indexer_revision IS NOT OLD.indexer_revision OR NEW.fingerprint IS NOT OLD.fingerprint OR NEW.title IS NOT OLD.title OR NEW.created_at IS NOT OLD.created_at OR (NEW.command_id IS NOT OLD.command_id AND NEW.command_id IS NOT NULL) THEN RAISE(ABORT,'candidate identity is immutable') END;
 SELECT CASE WHEN NOT ((NEW.command_id IS NULL AND OLD.command_id IS NOT NULL AND NEW.id IS OLD.id AND NEW.media_type IS OLD.media_type AND NEW.indexer_id IS OLD.indexer_id AND NEW.indexer_revision IS OLD.indexer_revision AND NEW.client_id IS OLD.client_id AND NEW.client_revision IS OLD.client_revision AND NEW.fingerprint IS OLD.fingerprint AND NEW.title IS OLD.title AND NEW.created_at IS OLD.created_at AND NEW.private_payload IS OLD.private_payload AND NEW.series_id IS OLD.series_id AND NEW.movie_id IS OLD.movie_id AND NEW.status IS OLD.status AND NEW.decision_reasons_json IS OLD.decision_reasons_json AND NEW.not_before IS OLD.not_before AND NEW.submission_identity_json IS OLD.submission_identity_json AND NEW.observed_hash IS OLD.observed_hash AND NEW.attempts IS OLD.attempts AND NEW.updated_at IS OLD.updated_at AND NEW.error_code IS OLD.error_code) OR (OLD.selection_kind!='legacy_fixed' AND NEW.status IN ('pending','rejected','cancelled') AND EXISTS(SELECT 1 FROM rss_client_attempts a WHERE a.candidate_id=OLD.id AND a.ordinal=OLD.active_attempt AND a.state='definitely_not_submitted') AND NOT EXISTS(SELECT 1 FROM rss_hash_claims h WHERE h.candidate_id=OLD.id AND h.client_id=OLD.client_id)) OR (OLD.status='pending' AND NEW.status IN ('pending','rejected','prepared','cancelled')) OR (OLD.status='prepared' AND NEW.status IN ('prepared','submitting','rejected','cancelled')) OR (OLD.status='submitting' AND NEW.status='pending' AND EXISTS(SELECT 1 FROM rss_client_attempts a WHERE a.candidate_id=OLD.id AND a.ordinal=OLD.active_attempt AND a.state='prepared' AND a.error_code='local_storage_retry')) OR (OLD.status='submitting' AND NEW.status IN ('reconciling','observed','needs_attention')) OR (OLD.status='reconciling' AND NEW.status IN ('reconciling','needs_attention'))) THEN RAISE(ABORT,'invalid RSS candidate transition') END;
 SELECT CASE WHEN OLD.status!='pending' AND (NEW.series_id IS NOT OLD.series_id OR NEW.movie_id IS NOT OLD.movie_id OR (NEW.submission_identity_json IS NOT OLD.submission_identity_json AND NOT (NEW.status='pending' AND NEW.submission_identity_json IS NULL AND EXISTS(SELECT 1 FROM rss_client_attempts a WHERE a.candidate_id=OLD.id AND a.ordinal=OLD.active_attempt AND a.state='definitely_not_submitted')))) THEN RAISE(ABORT,'prepared RSS target is immutable') END;
 SELECT CASE WHEN NEW.private_payload IS NOT OLD.private_payload AND NOT (NEW.private_payload IS NULL AND ((OLD.status='prepared' AND NEW.status='submitting') OR NEW.status IN ('rejected','cancelled') OR (OLD.selection_kind!='legacy_fixed' AND NEW.status IN ('observed','reconciling','needs_attention')))) THEN RAISE(ABORT,'RSS payload is immutable until dispatch') END;
 SELECT CASE WHEN NEW.attempts!=OLD.attempts+CASE WHEN NEW.status='reconciling' AND OLD.status IN ('submitting','reconciling') THEN 1 ELSE 0 END THEN RAISE(ABORT,'invalid reconciliation attempt counter') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM json_each(NEW.decision_reasons_json) WHERE type!='text' OR length(value)>128 OR value GLOB '*[^a-z0-9_]*') THEN RAISE(ABORT,'invalid decision reason') END;
 SELECT CASE WHEN NEW.status='prepared' AND ((NEW.media_type='tv' AND (SELECT count(*) FROM rss_candidate_episodes WHERE candidate_id=NEW.id)!=1) OR (NEW.media_type='movies' AND EXISTS(SELECT 1 FROM rss_candidate_episodes WHERE candidate_id=NEW.id))) THEN RAISE(ABORT,'unsupported dispatch target membership') END;
 SELECT CASE WHEN NEW.status='prepared' AND (json_extract(NEW.submission_identity_json,'$.version') IS NOT 1 OR json_extract(NEW.submission_identity_json,'$.target.media_type') IS NOT CASE WHEN NEW.media_type='tv' THEN 'episode' ELSE 'movie' END OR json_extract(NEW.submission_identity_json,'$.target.id') IS NOT CASE WHEN NEW.media_type='tv' THEN (SELECT episode_id FROM rss_candidate_episodes WHERE candidate_id=NEW.id) ELSE NEW.movie_id END OR json_type(NEW.submission_identity_json,'$.hashes') IS NOT 'array' OR json_array_length(NEW.submission_identity_json,'$.hashes') NOT BETWEEN 1 AND 2 OR EXISTS(SELECT 1 FROM json_each(NEW.submission_identity_json,'$.hashes') WHERE type!='text' OR length(value) NOT IN (40,64) OR value GLOB '*[^0-9a-f]*') OR (SELECT count(DISTINCT value) FROM json_each(NEW.submission_identity_json,'$.hashes'))!=json_array_length(NEW.submission_identity_json,'$.hashes')) THEN RAISE(ABORT,'invalid submission identity') END;
 SELECT CASE WHEN NEW.status='submitting' AND EXISTS(SELECT 1 FROM json_each(NEW.submission_identity_json,'$.hashes') j WHERE NOT EXISTS(SELECT 1 FROM rss_hash_claims h WHERE h.client_id=NEW.client_id AND h.hash=j.value AND h.candidate_id=NEW.id)) THEN RAISE(ABORT,'dispatch requires complete hash ownership') END;
END;

CREATE TRIGGER rss_candidate_delete BEFORE DELETE ON rss_candidates WHEN OLD.status NOT IN ('rejected','cancelled') BEGIN SELECT RAISE(ABORT,'only unclaimed rejected or cancelled candidates may be removed'); END;

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

CREATE TRIGGER rss_movie_target_owned BEFORE UPDATE OF status ON rss_candidates
WHEN NEW.status='prepared' AND EXISTS(SELECT 1 FROM rss_candidates owner WHERE owner.id!=NEW.id AND owner.movie_id=NEW.movie_id AND owner.status IN ('prepared','submitting','reconciling','observed','needs_attention') AND NOT EXISTS(SELECT 1 FROM rss_candidate_imports i JOIN import_journal j ON j.operation_id=i.operation_id JOIN import_history h ON h.operation_id=j.operation_id WHERE i.candidate_id=owner.id AND j.phase='complete'))
BEGIN SELECT RAISE(ABORT,'movie target already owned'); END;

CREATE TRIGGER rss_episode_target_owned BEFORE UPDATE OF status ON rss_candidates
WHEN NEW.status='prepared' AND EXISTS(SELECT 1 FROM rss_candidate_episodes incoming JOIN rss_candidate_episodes existing ON existing.episode_id=incoming.episode_id JOIN rss_candidates owner ON owner.id=existing.candidate_id WHERE incoming.candidate_id=NEW.id AND owner.id!=NEW.id AND owner.status IN ('prepared','submitting','reconciling','observed','needs_attention') AND NOT EXISTS(SELECT 1 FROM rss_candidate_imports i JOIN import_journal j ON j.operation_id=i.operation_id JOIN import_history h ON h.operation_id=j.operation_id WHERE i.candidate_id=owner.id AND j.phase='complete'))
BEGIN SELECT RAISE(ABORT,'episode target already owned'); END;

CREATE INDEX search_commands_ready ON search_commands(status,next_attempt_at,priority,created_at,id);

CREATE INDEX search_commands_history ON search_commands(created_at,id);

CREATE TRIGGER search_commands_transition BEFORE UPDATE ON search_commands BEGIN
 SELECT CASE WHEN NEW.selection_kind IS NOT OLD.selection_kind OR NEW.id IS NOT OLD.id OR NEW.mode IS NOT OLD.mode OR NEW.decision_context IS NOT OLD.decision_context OR NEW.requested_episode_id IS NOT OLD.requested_episode_id OR NEW.requested_movie_id IS NOT OLD.requested_movie_id OR NEW.captured_target_json IS NOT OLD.captured_target_json OR NEW.media_type IS NOT OLD.media_type OR NEW.indexer_id IS NOT OLD.indexer_id OR NEW.indexer_revision IS NOT OLD.indexer_revision OR NEW.client_id IS NOT OLD.client_id OR NEW.client_revision IS NOT OLD.client_revision OR NEW.priority IS NOT OLD.priority OR NEW.created_at IS NOT OLD.created_at THEN RAISE(ABORT,'command identity is immutable') END;
 SELECT CASE WHEN NOT ((OLD.status IN ('queued','retry_wait') AND NEW.status IN ('running','cancelled','failed')) OR (OLD.status='running' AND NEW.status IN ('running','retry_wait','succeeded','failed','cancelled'))) THEN RAISE(ABORT,'invalid command transition') END;
 SELECT CASE WHEN NEW.attempts != OLD.attempts + CASE WHEN NEW.status='running' AND OLD.status IN ('queued','retry_wait') THEN 1 ELSE 0 END THEN RAISE(ABORT,'invalid command attempt counter') END;
 SELECT CASE WHEN NEW.fetch_complete<OLD.fetch_complete OR (NEW.fetch_complete>OLD.fetch_complete AND NEW.status!='running') THEN RAISE(ABORT,'invalid search fetch checkpoint') END;
END;

CREATE UNIQUE INDEX candidate_search_result ON rss_candidates(search_result_id) WHERE search_result_id IS NOT NULL;

CREATE TRIGGER search_commands_delete BEFORE DELETE ON search_commands
WHEN OLD.status IN ('queued','running','retry_wait') OR EXISTS(SELECT 1 FROM search_results WHERE command_id=OLD.id AND selected_candidate_id IS NOT NULL)
BEGIN SELECT RAISE(ABORT,'selected or active search command must be retained'); END;

CREATE TRIGGER rss_candidate_admit BEFORE INSERT ON rss_candidates BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM rss_candidates)>=1024 THEN RAISE(ABORT,'RSS candidate capacity reached') END;
 -- A selected offer is transferred by the mandatory AFTER trigger in this same statement.
 SELECT CASE WHEN (SELECT coalesce(sum(length(private_payload)),0) FROM rss_candidates)+(SELECT coalesce(sum(length(private_payload)),0) FROM search_results)+coalesce(length(NEW.private_payload),0)-coalesce((SELECT length(private_payload) FROM search_results WHERE id=NEW.search_result_id AND selected_candidate_id IS NULL),0)>16777216 THEN RAISE(ABORT,'release payload capacity reached') END;
 SELECT CASE WHEN NEW.status NOT IN ('pending','rejected') OR NEW.attempts!=0 OR NEW.submission_identity_json IS NOT NULL THEN RAISE(ABORT,'invalid initial RSS candidate state') END;
 SELECT CASE WHEN NEW.search_result_id IS NULL AND NOT EXISTS(SELECT 1 FROM rss_commands c WHERE c.id=NEW.command_id AND c.media_type=NEW.media_type AND c.indexer_id=NEW.indexer_id AND c.indexer_revision=NEW.indexer_revision AND c.selection_kind=NEW.selection_kind AND c.client_id IS NEW.client_id AND c.client_revision IS NEW.client_revision) THEN RAISE(ABORT,'candidate requires matching RSS command') END;
 SELECT CASE WHEN NEW.search_result_id IS NOT NULL AND (NEW.command_id IS NOT NULL OR NEW.status!='pending' OR NOT EXISTS(SELECT 1 FROM search_results o JOIN search_commands c ON c.id=o.command_id WHERE o.id=NEW.search_result_id AND o.selected_candidate_id IS NULL AND o.private_payload IS NOT NULL AND o.expires_at>unixepoch() AND c.fetch_complete=1 AND ((c.mode='automatic' AND c.status='running') OR (c.mode='interactive' AND c.status='succeeded')) AND c.media_type=NEW.media_type AND c.indexer_id=NEW.indexer_id AND c.indexer_revision=NEW.indexer_revision AND c.selection_kind=NEW.selection_kind AND c.client_id IS NEW.client_id AND c.client_revision IS NEW.client_revision AND o.fingerprint=NEW.fingerprint AND o.title=NEW.title AND ((c.media_type='movies' AND NEW.movie_id=c.requested_movie_id) OR (c.media_type='tv' AND NEW.series_id=json_extract(c.captured_target_json,'$.series_id'))))) THEN RAISE(ABORT,'candidate requires selectable captured search result') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM json_each(NEW.decision_reasons_json) WHERE type!='text' OR length(value)>128 OR value GLOB '*[^a-z0-9_]*') THEN RAISE(ABORT,'invalid decision reason') END;
END;

CREATE TRIGGER search_result_transfer AFTER INSERT ON rss_candidates WHEN NEW.search_result_id IS NOT NULL BEGIN
 UPDATE search_results SET selected_candidate_id=NEW.id,private_payload=NULL WHERE id=NEW.search_result_id;
END;

CREATE TRIGGER search_candidate_origin BEFORE UPDATE OF search_result_id ON rss_candidates
WHEN NEW.search_result_id IS NOT OLD.search_result_id
BEGIN SELECT RAISE(ABORT,'candidate origin is immutable'); END;

CREATE TRIGGER search_candidate_target BEFORE UPDATE OF series_id,movie_id ON rss_candidates
WHEN OLD.search_result_id IS NOT NULL AND (NEW.series_id IS NOT OLD.series_id OR NEW.movie_id IS NOT OLD.movie_id)
BEGIN SELECT RAISE(ABORT,'requested search target is immutable'); END;

CREATE TRIGGER rss_comparison_facts_insert BEFORE INSERT ON rss_candidates
WHEN NEW.comparison_facts_json IS NOT NULL
BEGIN SELECT RAISE(ABORT,'comparison facts require submission transition'); END;

CREATE TRIGGER rss_comparison_facts_update BEFORE UPDATE ON rss_candidates
WHEN NEW.comparison_facts_json IS NOT OLD.comparison_facts_json
 AND NOT (OLD.status='prepared' AND NEW.status='submitting'
          AND OLD.comparison_facts_json IS NULL AND NEW.comparison_facts_json IS NOT NULL)
 AND NOT (OLD.status='submitting' AND NEW.status='pending' AND NEW.error_code='local_storage_retry' AND NEW.comparison_facts_json IS NULL)
 AND NOT (OLD.selection_kind='default' AND NEW.status='pending' AND NEW.comparison_facts_json IS NULL AND EXISTS(SELECT 1 FROM rss_client_attempts a WHERE a.candidate_id=OLD.id AND a.ordinal=OLD.active_attempt AND a.state='definitely_not_submitted'))
BEGIN SELECT RAISE(ABORT,'submitted comparison facts are immutable'); END;

CREATE TRIGGER rss_commands_transition BEFORE UPDATE ON rss_commands BEGIN
 SELECT CASE WHEN NEW.selection_kind IS NOT OLD.selection_kind OR NEW.id IS NOT OLD.id OR NEW.name IS NOT OLD.name OR NEW.media_type IS NOT OLD.media_type OR NEW.indexer_id IS NOT OLD.indexer_id OR NEW.indexer_revision IS NOT OLD.indexer_revision OR NEW.client_id IS NOT OLD.client_id OR NEW.client_revision IS NOT OLD.client_revision OR NEW.priority IS NOT OLD.priority OR NEW.created_at IS NOT OLD.created_at THEN RAISE(ABORT,'command identity is immutable') END;
 SELECT CASE WHEN NOT ((OLD.status IN ('queued','retry_wait') AND NEW.next_attempt_at=0 AND OLD.next_attempt_at>0 AND NEW.id IS OLD.id AND NEW.name IS OLD.name AND NEW.media_type IS OLD.media_type AND NEW.priority IS OLD.priority AND NEW.status IS OLD.status AND NEW.attempts IS OLD.attempts AND NEW.created_at IS OLD.created_at AND NEW.started_at IS OLD.started_at AND NEW.completed_at IS OLD.completed_at AND NEW.indexer_id IS OLD.indexer_id AND NEW.indexer_revision IS OLD.indexer_revision AND NEW.client_id IS OLD.client_id AND NEW.client_revision IS OLD.client_revision AND NEW.error_code IS OLD.error_code AND NEW.fetched IS OLD.fetched AND NEW.evaluated IS OLD.evaluated AND NEW.rejected IS OLD.rejected AND NEW.pending IS OLD.pending AND NEW.observed IS OLD.observed AND NEW.uncertain IS OLD.uncertain AND NEW.fetch_complete IS OLD.fetch_complete) OR (OLD.status IN ('queued','retry_wait') AND NEW.status IN ('running','cancelled','failed')) OR (OLD.status='running' AND NEW.status IN ('running','retry_wait','succeeded','failed','cancelled'))) THEN RAISE(ABORT,'invalid command transition') END;
 SELECT CASE WHEN NEW.attempts != OLD.attempts + CASE WHEN NEW.status='running' AND OLD.status IN ('queued','retry_wait') THEN 1 ELSE 0 END THEN RAISE(ABORT,'invalid command attempt counter') END;
 SELECT CASE WHEN NEW.fetch_complete<OLD.fetch_complete OR (NEW.fetch_complete>OLD.fetch_complete AND NEW.status!='running') THEN RAISE(ABORT,'invalid RSS fetch checkpoint') END;
END;

CREATE TRIGGER rss_commands_admit BEFORE INSERT ON rss_commands BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 OR NEW.fetch_complete!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT (EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.indexer_id AND p.revision=NEW.indexer_revision AND p.implementation IN ('torznab','newznab') AND s.media_type=NEW.media_type AND p.enabled=1) AND (NEW.selection_kind='default' OR EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.client_id AND p.revision=NEW.client_revision AND p.implementation='qbittorrent' AND s.media_type=NEW.media_type AND p.enabled=1))) THEN RAISE(ABORT,'invalid RSS provider scope or revision') END;
END;

CREATE TRIGGER search_commands_admit BEFORE INSERT ON search_commands BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 OR NEW.fetch_complete!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT (EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.indexer_id AND p.revision=NEW.indexer_revision AND p.implementation IN ('torznab','newznab') AND s.media_type=NEW.media_type AND p.enabled=1) AND (NEW.selection_kind='default' OR EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.client_id AND p.revision=NEW.client_revision AND p.implementation='qbittorrent' AND s.media_type=NEW.media_type AND p.enabled=1))) THEN RAISE(ABORT,'invalid RSS provider scope or revision') END;
 SELECT CASE WHEN NOT ((NEW.media_type='tv' AND EXISTS(SELECT 1 FROM episodes e WHERE e.id=NEW.requested_episode_id AND json_extract(NEW.captured_target_json,'$.media_type')='tv' AND json_extract(NEW.captured_target_json,'$.episode_id')=e.id AND json_extract(NEW.captured_target_json,'$.series_id')=e.series_id)) OR (NEW.media_type='movies' AND EXISTS(SELECT 1 FROM movies m WHERE m.id=NEW.requested_movie_id AND json_extract(NEW.captured_target_json,'$.media_type')='movies' AND json_extract(NEW.captured_target_json,'$.movie_id')=m.id AND json_extract(NEW.captured_target_json,'$.metadata_id')=m.metadata_id))) THEN RAISE(ABORT,'invalid search target') END;
END;

CREATE UNIQUE INDEX rss_schedules_selection ON rss_schedules(indexer_id,selection_kind,coalesce(client_id,''),media_type);

CREATE UNIQUE INDEX rss_candidate_selection_identity ON rss_candidates(indexer_id,indexer_revision,selection_kind,coalesce(CASE WHEN selection_kind!='default' THEN client_id END,''),coalesce(CASE WHEN selection_kind!='default' THEN client_revision END,0),media_type,fingerprint);

CREATE TABLE download_client_tags (
 provider_id TEXT NOT NULL,
 media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
 tag_id INTEGER NOT NULL,
 PRIMARY KEY(provider_id,media_type,tag_id),
 FOREIGN KEY(provider_id,media_type) REFERENCES provider_scopes(provider_id,media_type) ON DELETE CASCADE,
 FOREIGN KEY(tag_id,media_type) REFERENCES tags(id,media_type) ON DELETE RESTRICT
);
CREATE INDEX download_client_tags_tag ON download_client_tags(tag_id,media_type,provider_id);
CREATE TRIGGER download_client_tags_admit BEFORE INSERT ON download_client_tags BEGIN
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM provider_scopes WHERE provider_id=NEW.provider_id AND media_type=NEW.media_type AND implementation='qbittorrent') THEN RAISE(ABORT,'download client tag requires scoped client') END;
 SELECT CASE WHEN (SELECT count(*) FROM download_client_tags WHERE provider_id=NEW.provider_id AND media_type=NEW.media_type)>=200 THEN RAISE(ABORT,'download client tag capacity reached') END;
END;
CREATE TRIGGER download_client_tags_immutable BEFORE UPDATE ON download_client_tags BEGIN SELECT RAISE(ABORT,'replace download client tags explicitly'); END;

CREATE TABLE rss_client_attempts (
 candidate_id TEXT NOT NULL REFERENCES rss_candidates(id) ON DELETE CASCADE,
 ordinal INTEGER NOT NULL CHECK(typeof(ordinal)='integer' AND ordinal BETWEEN 1 AND 128),
 selection_round INTEGER NOT NULL DEFAULT 1 CHECK(typeof(selection_round)='integer' AND selection_round BETWEEN 1 AND 128),
 client_id TEXT NOT NULL CHECK(length(client_id)=36 AND substr(client_id,9,1)='-' AND substr(client_id,14,1)='-' AND substr(client_id,19,1)='-' AND substr(client_id,24,1)='-' AND length(replace(client_id,'-',''))=32 AND replace(client_id,'-','') NOT GLOB '*[^0-9a-f]*'),
 client_revision INTEGER NOT NULL CHECK(typeof(client_revision)='integer' AND client_revision BETWEEN 1 AND 9007199254740991),
 state TEXT NOT NULL CHECK(state IN ('selected','prepared','dispatching','definitely_not_submitted','observed','unknown')),
 identity_json TEXT CHECK(identity_json IS NULL OR (typeof(identity_json)='text' AND length(CAST(identity_json AS BLOB))<=4096 AND json_valid(identity_json) AND json_type(identity_json)='object' AND json_type(identity_json,'$.payload_sha256') IS 'text' AND length(json_extract(identity_json,'$.payload_sha256'))=64 AND json_extract(identity_json,'$.payload_sha256') NOT GLOB '*[^0-9a-f]*')),
 error_code TEXT CHECK(error_code IS NULL OR (typeof(error_code)='text' AND length(error_code) BETWEEN 1 AND 128 AND error_code NOT GLOB '*[^a-z0-9_]*')),
 created_at INTEGER NOT NULL CHECK(typeof(created_at)='integer' AND created_at BETWEEN 0 AND 9007199254740991),
 updated_at INTEGER NOT NULL CHECK(typeof(updated_at)='integer' AND updated_at BETWEEN created_at AND 9007199254740991),
 PRIMARY KEY(candidate_id,ordinal),
 UNIQUE(candidate_id,selection_round,client_id),
 CHECK(state NOT IN ('prepared','dispatching','observed','unknown') OR identity_json IS NOT NULL),
 CHECK((state IN ('definitely_not_submitted','unknown') AND error_code IS NOT NULL) OR (state='prepared' AND error_code IS 'local_storage_retry') OR (state NOT IN ('definitely_not_submitted','unknown') AND error_code IS NULL))
);
CREATE UNIQUE INDEX rss_client_attempt_active ON rss_client_attempts(candidate_id) WHERE state IN ('selected','prepared','dispatching');
CREATE TRIGGER rss_client_attempt_admit BEFORE INSERT ON rss_client_attempts BEGIN
 SELECT CASE WHEN NEW.state!='selected' OR NEW.identity_json IS NOT NULL OR NEW.error_code IS NOT NULL OR NEW.created_at!=NEW.updated_at THEN RAISE(ABORT,'client attempt must start selected') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM rss_candidates c,json_each(c.selection_plan_json) p WHERE c.id=NEW.candidate_id AND c.selection_round=NEW.selection_round AND c.selection_kind!='legacy_fixed' AND c.status='pending' AND c.active_attempt IS NULL AND c.private_payload IS NOT NULL AND c.payload_expires_at>NEW.created_at AND json_extract(p.value,'$.client_id')=NEW.client_id AND json_extract(p.value,'$.client_revision')=NEW.client_revision) THEN RAISE(ABORT,'client attempt requires unexpired selection plan') END;
 SELECT CASE WHEN NEW.ordinal!=(SELECT coalesce(max(ordinal),0)+1 FROM rss_client_attempts WHERE candidate_id=NEW.candidate_id) OR EXISTS(SELECT 1 FROM rss_client_attempts WHERE candidate_id=NEW.candidate_id AND state!='definitely_not_submitted') THEN RAISE(ABORT,'previous client attempts prohibit another selection') END;
END;
CREATE TRIGGER rss_client_attempt_transition BEFORE UPDATE ON rss_client_attempts BEGIN
 SELECT CASE WHEN NEW.candidate_id IS NOT OLD.candidate_id OR NEW.selection_round IS NOT OLD.selection_round OR NEW.ordinal IS NOT OLD.ordinal OR NEW.client_id IS NOT OLD.client_id OR NEW.client_revision IS NOT OLD.client_revision OR NEW.created_at IS NOT OLD.created_at OR NEW.updated_at<OLD.updated_at THEN RAISE(ABORT,'client attempt identity is immutable') END;
 SELECT CASE WHEN NOT ((OLD.state='selected' AND NEW.state IN ('prepared','definitely_not_submitted')) OR (OLD.state='prepared' AND NEW.state IN ('dispatching','definitely_not_submitted')) OR (OLD.state='dispatching' AND NEW.state IN ('definitely_not_submitted','observed','unknown')) OR (OLD.state='dispatching' AND NEW.state='prepared' AND NEW.error_code IS 'local_storage_retry')) THEN RAISE(ABORT,'invalid client attempt transition') END;
 -- Only an explicit no-add local-storage result can return the same attempt to preparation.
 SELECT CASE WHEN NEW.state='prepared' AND NEW.error_code IS 'local_storage_retry' AND NOT (
  OLD.state='dispatching' AND EXISTS(SELECT 1 FROM rss_candidates c WHERE c.id=OLD.candidate_id
   AND c.status='submitting' AND c.selection_kind!='legacy_fixed' AND c.active_attempt=OLD.ordinal
   AND c.selection_round=OLD.selection_round AND c.client_id=OLD.client_id AND c.client_revision=OLD.client_revision
   AND c.submission_identity_json=OLD.identity_json AND c.private_payload IS NOT NULL AND c.payload_expires_at>NEW.updated_at)
  AND NOT EXISTS(SELECT 1 FROM rss_client_attempts a WHERE a.candidate_id=OLD.candidate_id AND a.state IN ('unknown','observed'))
 ) THEN RAISE(ABORT,'local retry requires exact safe dispatched attempt') END;
 SELECT CASE WHEN NEW.identity_json IS NOT OLD.identity_json AND NOT (OLD.state='selected' AND NEW.state='prepared' AND OLD.identity_json IS NULL AND NEW.identity_json IS NOT NULL) THEN RAISE(ABORT,'prepared client attempt identity is immutable') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM rss_candidates c WHERE c.id=NEW.candidate_id AND c.selection_round=NEW.selection_round AND c.active_attempt=NEW.ordinal AND c.client_id=NEW.client_id AND c.client_revision=NEW.client_revision AND c.status IN ('pending','prepared','submitting')) THEN RAISE(ABORT,'client attempt requires matching candidate projection') END;
 SELECT CASE WHEN NEW.state='dispatching' AND NOT EXISTS(SELECT 1 FROM rss_candidates c WHERE c.id=NEW.candidate_id AND c.status='prepared' AND c.submission_identity_json=NEW.identity_json AND c.payload_expires_at>NEW.updated_at AND NOT EXISTS(SELECT 1 FROM json_each(NEW.identity_json,'$.hashes') j WHERE NOT EXISTS(SELECT 1 FROM rss_hash_claims h WHERE h.candidate_id=c.id AND h.client_id=NEW.client_id AND h.hash=j.value))) THEN RAISE(ABORT,'dispatch requires prepared identity and hash ownership') END;
 SELECT CASE WHEN NEW.state IN ('observed','unknown') AND NOT EXISTS(SELECT 1 FROM rss_candidates WHERE id=NEW.candidate_id AND status='submitting') THEN RAISE(ABORT,'client result requires dispatched candidate') END;
 SELECT CASE WHEN NEW.state='prepared' AND EXISTS(SELECT 1 FROM rss_client_attempts a WHERE a.candidate_id=NEW.candidate_id AND a.identity_json IS NOT NULL AND json_extract(a.identity_json,'$.payload_sha256') IS NOT json_extract(NEW.identity_json,'$.payload_sha256')) THEN RAISE(ABORT,'fallback release bytes changed') END;
END;
CREATE TRIGGER rss_client_attempt_delete BEFORE DELETE ON rss_client_attempts
WHEN EXISTS(SELECT 1 FROM rss_candidates WHERE id=OLD.candidate_id)
BEGIN SELECT RAISE(ABORT,'client attempt evidence belongs to retained candidate'); END;

CREATE TRIGGER rss_candidate_selection_admit BEFORE INSERT ON rss_candidates BEGIN
 SELECT CASE WHEN NEW.selection_round!=0 OR NEW.selection_plan_json IS NOT NULL OR NEW.active_attempt IS NOT NULL OR NEW.selection_strict!=0 OR NEW.payload_expires_at IS NOT NULL THEN RAISE(ABORT,'candidate selection starts unresolved') END;
END;
CREATE TRIGGER rss_candidate_selection_transition BEFORE UPDATE ON rss_candidates BEGIN
 -- Pending unavailable is automation authority, never an implicit interactive retry.
 SELECT CASE WHEN NEW.selection_strict<OLD.selection_strict OR (NEW.selection_strict>OLD.selection_strict AND NOT (
  OLD.selection_kind='default' AND NEW.selection_plan_json IS NULL AND NEW.active_attempt IS NULL
  AND NEW.status='pending' AND NEW.error_code IS 'download_client_unavailable' AND NEW.not_before IS NOT NULL AND NEW.not_before>NEW.updated_at
  AND EXISTS(SELECT 1 FROM json_each(NEW.decision_reasons_json) WHERE value='download_client_unavailable')
  AND (OLD.search_result_id IS NULL OR EXISTS(SELECT 1 FROM search_results r JOIN search_commands c ON c.id=r.command_id WHERE r.id=OLD.search_result_id AND c.mode='automatic'))
  AND NOT EXISTS(SELECT 1 FROM rss_client_attempts WHERE candidate_id=OLD.id AND state!='definitely_not_submitted')
  AND NOT EXISTS(SELECT 1 FROM rss_hash_claims WHERE candidate_id=OLD.id)
 )) THEN RAISE(ABORT,'strict selection requires unavailable automation pending reason') END;
 SELECT CASE WHEN NEW.selection_round IS NOT OLD.selection_round AND NOT (
  NEW.selection_round=OLD.selection_round+1 AND OLD.selection_plan_json IS NULL AND NEW.selection_plan_json IS NOT NULL
 ) THEN RAISE(ABORT,'selection round advances only with a captured plan') END;
 SELECT CASE WHEN NEW.selection_plan_json IS NOT OLD.selection_plan_json AND NOT (
  -- A capture consumes a round, never a fresh payload-retention window after the first round.
  (OLD.selection_kind!='legacy_fixed' AND OLD.status='pending' AND NEW.status='pending'
   AND OLD.selection_plan_json IS NULL AND NEW.selection_plan_json IS NOT NULL
   AND OLD.active_attempt IS NULL AND NEW.active_attempt IS NULL AND NEW.selection_round=OLD.selection_round+1
   AND NEW.payload_expires_at>NEW.updated_at
   AND (OLD.selection_strict=0 OR (OLD.not_before IS NOT NULL AND OLD.not_before<=NEW.updated_at))
   AND ((OLD.selection_round=0 AND OLD.payload_expires_at IS NULL AND NEW.payload_expires_at=NEW.updated_at+1800)
    OR (OLD.selection_round>0 AND OLD.selection_kind='default' AND OLD.selection_strict=1
     AND OLD.error_code IS 'download_client_unavailable' AND OLD.not_before IS NOT NULL AND OLD.not_before<=NEW.updated_at
     AND NEW.payload_expires_at=OLD.payload_expires_at
     AND (OLD.search_result_id IS NULL OR EXISTS(SELECT 1 FROM search_results r JOIN search_commands c ON c.id=r.command_id WHERE r.id=OLD.search_result_id AND c.mode='automatic'))))
   AND json_array_length(NEW.selection_plan_json)+(SELECT count(*) FROM rss_client_attempts WHERE candidate_id=OLD.id)<=128
   AND NOT EXISTS(SELECT 1 FROM rss_client_attempts WHERE candidate_id=OLD.id AND state!='definitely_not_submitted')
   AND NOT EXISTS(SELECT 1 FROM rss_hash_claims WHERE candidate_id=OLD.id))
  OR
  -- Every planned identity must survive as immutable safe evidence before that plan can clear.
  (OLD.selection_kind='default' AND OLD.selection_plan_json IS NOT NULL AND NEW.selection_plan_json IS NULL
   AND NEW.selection_round=OLD.selection_round AND NEW.status='pending' AND NEW.selection_strict=1
   AND NEW.error_code IS 'download_client_unavailable' AND NEW.not_before IS NOT NULL AND NEW.not_before>NEW.updated_at
   AND EXISTS(SELECT 1 FROM json_each(NEW.decision_reasons_json) WHERE value='download_client_unavailable')
   AND NEW.active_attempt IS NULL AND NEW.client_id IS NULL AND NEW.client_revision IS NULL
   AND NEW.submission_identity_json IS NULL AND NEW.comparison_facts_json IS NULL
   AND NEW.payload_expires_at=OLD.payload_expires_at AND NEW.payload_expires_at>NEW.updated_at
   AND (OLD.search_result_id IS NULL OR EXISTS(SELECT 1 FROM search_results r JOIN search_commands c ON c.id=r.command_id WHERE r.id=OLD.search_result_id AND c.mode='automatic'))
   AND (SELECT count(*) FROM rss_client_attempts WHERE candidate_id=OLD.id)<128
   AND NOT EXISTS(SELECT 1 FROM json_each(OLD.selection_plan_json) p WHERE NOT EXISTS(
    SELECT 1 FROM rss_client_attempts a WHERE a.candidate_id=OLD.id AND a.selection_round=OLD.selection_round
     AND a.client_id=json_extract(p.value,'$.client_id') AND a.client_revision=json_extract(p.value,'$.client_revision') AND a.state='definitely_not_submitted'))
   AND NOT EXISTS(SELECT 1 FROM rss_client_attempts WHERE candidate_id=OLD.id AND state!='definitely_not_submitted')
   AND NOT EXISTS(SELECT 1 FROM rss_hash_claims WHERE candidate_id=OLD.id))
 ) THEN RAISE(ABORT,'selection plan requires a fresh round or safe unavailable exhaustion') END;
 SELECT CASE WHEN OLD.selection_plan_json IS NULL AND NEW.selection_plan_json IS NOT NULL AND NEW.selection_kind='default' AND EXISTS(SELECT 1 FROM provider_scopes s WHERE s.provider_id=NEW.indexer_id AND s.media_type=NEW.media_type AND s.download_client_id IS NOT NULL AND (json_array_length(NEW.selection_plan_json)!=1 OR json_extract(NEW.selection_plan_json,'$[0].client_id') IS NOT s.download_client_id)) THEN RAISE(ABORT,'bound default has exactly its preferred client') END;
 SELECT CASE WHEN NEW.payload_expires_at IS NOT OLD.payload_expires_at AND NOT (OLD.selection_plan_json IS NULL AND NEW.selection_plan_json IS NOT NULL AND OLD.payload_expires_at IS NULL AND NEW.payload_expires_at=NEW.updated_at+1800) THEN RAISE(ABORT,'selected payload expiry is immutable') END;
 SELECT CASE WHEN NEW.selection_plan_json IS NOT NULL AND (EXISTS(SELECT 1 FROM json_each(NEW.selection_plan_json) p WHERE p.type!='object' OR (SELECT count(*) FROM json_each(p.value))!=2 OR EXISTS(SELECT 1 FROM json_each(p.value) WHERE key NOT IN ('client_id','client_revision')) OR json_type(p.value,'$.client_id') IS NOT 'text' OR length(json_extract(p.value,'$.client_id'))!=36 OR substr(json_extract(p.value,'$.client_id'),9,1)!='-' OR substr(json_extract(p.value,'$.client_id'),14,1)!='-' OR substr(json_extract(p.value,'$.client_id'),19,1)!='-' OR substr(json_extract(p.value,'$.client_id'),24,1)!='-' OR length(replace(json_extract(p.value,'$.client_id'),'-',''))!=32 OR replace(json_extract(p.value,'$.client_id'),'-','') GLOB '*[^0-9a-f]*' OR json_type(p.value,'$.client_revision') IS NOT 'integer' OR json_extract(p.value,'$.client_revision') NOT BETWEEN 1 AND 9007199254740991) OR (SELECT count(DISTINCT json_extract(value,'$.client_id')) FROM json_each(NEW.selection_plan_json))!=json_array_length(NEW.selection_plan_json) OR (NEW.media_type='movies' AND json_array_length(NEW.selection_plan_json)!=1) OR (NEW.selection_kind='explicit' AND (json_array_length(NEW.selection_plan_json)!=1 OR json_extract(NEW.selection_plan_json,'$[0].client_id') IS NOT NEW.client_id OR json_extract(NEW.selection_plan_json,'$[0].client_revision') IS NOT NEW.client_revision))) THEN RAISE(ABORT,'invalid ordered client selection plan') END;
 SELECT CASE WHEN OLD.selection_kind!='default' AND (NEW.client_id IS NOT OLD.client_id OR NEW.client_revision IS NOT OLD.client_revision) THEN RAISE(ABORT,'fixed client intent is immutable') END;
 SELECT CASE WHEN OLD.selection_kind='default' AND (NEW.client_id IS NOT OLD.client_id OR NEW.client_revision IS NOT OLD.client_revision) AND NOT ((OLD.client_id IS NULL AND OLD.active_attempt IS NULL AND OLD.status='pending' AND NEW.status='pending' AND NEW.client_id IS NOT NULL AND EXISTS(SELECT 1 FROM rss_client_attempts a WHERE a.candidate_id=OLD.id AND a.selection_round=NEW.selection_round AND a.ordinal=NEW.active_attempt AND a.state='selected' AND a.client_id=NEW.client_id AND a.client_revision=NEW.client_revision)) OR (OLD.client_id IS NOT NULL AND NEW.client_id IS NULL AND NEW.active_attempt IS NULL AND NEW.status='pending' AND NEW.submission_identity_json IS NULL AND NEW.comparison_facts_json IS NULL AND EXISTS(SELECT 1 FROM rss_client_attempts a WHERE a.candidate_id=OLD.id AND a.ordinal=OLD.active_attempt AND a.state='definitely_not_submitted') AND NOT EXISTS(SELECT 1 FROM rss_hash_claims WHERE candidate_id=OLD.id AND client_id=OLD.client_id))) THEN RAISE(ABORT,'client projection requires selected or safely settled attempt') END;
 SELECT CASE WHEN NEW.active_attempt IS NOT OLD.active_attempt AND NOT ((OLD.active_attempt IS NULL AND NEW.active_attempt IS NOT NULL AND OLD.status='pending' AND NEW.status='pending' AND EXISTS(SELECT 1 FROM rss_client_attempts a WHERE a.candidate_id=OLD.id AND a.selection_round=NEW.selection_round AND a.ordinal=NEW.active_attempt AND a.state='selected' AND a.client_id=NEW.client_id AND a.client_revision=NEW.client_revision)) OR (NEW.active_attempt IS NULL AND NEW.status='pending' AND NEW.client_id IS NULL AND EXISTS(SELECT 1 FROM rss_client_attempts a WHERE a.candidate_id=OLD.id AND a.ordinal=OLD.active_attempt AND a.state='definitely_not_submitted') AND NOT EXISTS(SELECT 1 FROM rss_hash_claims WHERE candidate_id=OLD.id AND client_id=OLD.client_id))) THEN RAISE(ABORT,'active attempt projection is immutable until safe settlement') END;
 SELECT CASE WHEN NEW.selection_kind!='legacy_fixed' AND NEW.status IN ('prepared','submitting','observed','reconciling','needs_attention') AND NOT EXISTS(SELECT 1 FROM rss_client_attempts a WHERE a.candidate_id=NEW.id AND a.selection_round=NEW.selection_round AND a.ordinal=NEW.active_attempt AND a.client_id=NEW.client_id AND a.client_revision=NEW.client_revision AND a.identity_json=NEW.submission_identity_json AND ((NEW.status='prepared' AND a.state='prepared') OR (NEW.status='submitting' AND a.state='dispatching') OR (NEW.status='observed' AND a.state='observed') OR (NEW.status IN ('reconciling','needs_attention') AND a.state='unknown'))) THEN RAISE(ABORT,'candidate requires matching durable client attempt') END;
 SELECT CASE WHEN NEW.selection_round>0 AND (NEW.series_id IS NOT OLD.series_id OR NEW.movie_id IS NOT OLD.movie_id) THEN RAISE(ABORT,'selected candidate target is immutable') END;
END;
CREATE TRIGGER rss_candidate_local_storage_retry BEFORE UPDATE ON rss_candidates BEGIN
 SELECT CASE WHEN OLD.status='submitting' AND NEW.status='pending' AND NOT (
  OLD.selection_kind!='legacy_fixed' AND NEW.command_id IS OLD.command_id AND NEW.error_code IS 'local_storage_retry'
  AND json_array_length(NEW.decision_reasons_json)=1 AND json_extract(NEW.decision_reasons_json,'$[0]') IS 'local_storage_retry' AND NEW.not_before IS NOT NULL AND NEW.not_before>NEW.updated_at
  AND NEW.updated_at>=OLD.updated_at AND NEW.private_payload IS OLD.private_payload AND NEW.private_payload IS NOT NULL
  AND NEW.payload_expires_at IS OLD.payload_expires_at AND NEW.payload_expires_at>NEW.updated_at
  AND NEW.client_id IS OLD.client_id AND NEW.client_revision IS OLD.client_revision AND NEW.active_attempt IS OLD.active_attempt
  AND NEW.selection_plan_json IS OLD.selection_plan_json AND NEW.selection_round IS OLD.selection_round AND NEW.selection_strict IS OLD.selection_strict
  AND NEW.submission_identity_json IS OLD.submission_identity_json AND NEW.comparison_facts_json IS NULL
  AND NEW.series_id IS OLD.series_id AND NEW.movie_id IS OLD.movie_id
  AND EXISTS(SELECT 1 FROM rss_client_attempts a WHERE a.candidate_id=OLD.id AND a.ordinal=OLD.active_attempt
   AND a.selection_round=OLD.selection_round AND a.client_id=OLD.client_id AND a.client_revision=OLD.client_revision
   AND a.identity_json=OLD.submission_identity_json AND a.state='prepared' AND a.error_code IS 'local_storage_retry')
  AND NOT EXISTS(SELECT 1 FROM rss_hash_claims WHERE candidate_id=OLD.id)
  AND NOT EXISTS(SELECT 1 FROM rss_client_attempts WHERE candidate_id=OLD.id AND state IN ('unknown','observed'))
 ) AND NOT EXISTS(SELECT 1 FROM rss_client_attempts WHERE candidate_id=OLD.id AND ordinal=OLD.active_attempt AND state='definitely_not_submitted')
 THEN RAISE(ABORT,'local retry must retain its exact release and future deadline') END;
 SELECT CASE WHEN OLD.status='pending' AND EXISTS(SELECT 1 FROM rss_client_attempts WHERE candidate_id=OLD.id AND ordinal=OLD.active_attempt AND state='prepared' AND error_code IS 'local_storage_retry')
  AND ((NEW.status='prepared' AND (OLD.not_before IS NULL OR OLD.not_before>NEW.updated_at))
   OR (NEW.status='pending' AND (NEW.not_before IS NULL OR NEW.not_before<OLD.not_before)))
 THEN RAISE(ABORT,'local retry deadline must be reached before preparation') END;
END;
CREATE TRIGGER rss_selected_episode_insert BEFORE INSERT ON rss_candidate_episodes
WHEN EXISTS(SELECT 1 FROM rss_candidates WHERE id=NEW.candidate_id AND selection_round>0)
BEGIN SELECT RAISE(ABORT,'selected episode membership is immutable'); END;
CREATE TRIGGER rss_selected_episode_delete BEFORE DELETE ON rss_candidate_episodes
WHEN EXISTS(SELECT 1 FROM rss_candidates WHERE id=OLD.candidate_id AND selection_round>0)
BEGIN SELECT RAISE(ABORT,'selected episode membership is immutable'); END;
DROP TRIGGER rss_hash_retained;
CREATE TRIGGER rss_hash_retained BEFORE DELETE ON rss_hash_claims
WHEN NOT EXISTS(SELECT 1 FROM rss_candidates c JOIN rss_client_attempts a ON a.candidate_id=c.id AND a.ordinal=c.active_attempt WHERE c.id=OLD.candidate_id AND a.selection_round=c.selection_round AND a.client_id=OLD.client_id AND (a.state='definitely_not_submitted' OR (a.state='prepared' AND a.error_code IS 'local_storage_retry' AND c.status='submitting' AND a.client_revision=c.client_revision AND a.identity_json=c.submission_identity_json)))
BEGIN SELECT RAISE(ABORT,'uncertain or observed hash ownership must be retained'); END;
