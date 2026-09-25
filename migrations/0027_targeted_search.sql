-- Typed user-invoked search and retained offer authority; all mutation reuses the grab journal.
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
 client_id TEXT NOT NULL,
 client_revision INTEGER NOT NULL CHECK(typeof(client_revision)='integer' AND client_revision BETWEEN 1 AND 9007199254740991),
 error_code TEXT CHECK(error_code IS NULL OR error_code IN ('interrupted','storage_error','provider_changed','provider_unavailable','refresh_timeout','refresh_limit','refresh_failed','key_unavailable','invalid_release','candidate_limit','hash_conflict','submission_unknown','not_observed','unsupported_target','target_conflict','target_changed','preexisting_download','presence_unconfirmed','command_history_full','command_conflict','search_limit','no_eligible_release','result_expired','invalid_search_target')),
 fetched INTEGER NOT NULL DEFAULT 0 CHECK(typeof(fetched)='integer' AND fetched BETWEEN 0 AND 1000),
 fetch_complete INTEGER NOT NULL DEFAULT 0 CHECK(typeof(fetch_complete)='integer' AND fetch_complete IN (0,1)),
 CHECK((status IN ('queued','running','retry_wait') AND completed_at IS NULL) OR (status IN ('succeeded','failed','cancelled') AND completed_at IS NOT NULL)),
 CHECK(status!='queued' OR (attempts=0 AND started_at IS NULL)),
 CHECK(status NOT IN ('running','succeeded','retry_wait') OR (attempts>0 AND started_at IS NOT NULL)),
 CHECK(status!='retry_wait' OR attempts<3),
 CHECK((status IN ('failed','retry_wait') AND error_code IS NOT NULL) OR (status NOT IN ('failed','retry_wait') AND error_code IS NULL)),
 CHECK(status!='succeeded' OR fetch_complete=1),
 CHECK((media_type='tv' AND requested_episode_id IS NOT NULL AND requested_movie_id IS NULL) OR (media_type='movies' AND requested_movie_id IS NOT NULL AND requested_episode_id IS NULL))
);
CREATE INDEX search_commands_ready ON search_commands(status,next_attempt_at,priority,created_at,id);
CREATE INDEX search_commands_history ON search_commands(created_at,id);
CREATE TRIGGER search_commands_admit BEFORE INSERT ON search_commands BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands)+(SELECT count(*) FROM metadata_refresh_commands)+(SELECT count(*) FROM blocklist_clear_commands)+(SELECT count(*) FROM rss_commands)+(SELECT count(*) FROM search_commands)>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 OR NEW.fetch_complete!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT (EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.indexer_id AND p.revision=NEW.indexer_revision AND p.implementation IN ('torznab','newznab') AND s.media_type=NEW.media_type AND p.enabled=1) AND EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.client_id AND p.revision=NEW.client_revision AND p.implementation='qbittorrent' AND s.media_type=NEW.media_type AND p.enabled=1)) THEN RAISE(ABORT,'invalid RSS provider scope or revision') END;
 SELECT CASE WHEN NOT ((NEW.media_type='tv' AND EXISTS(SELECT 1 FROM episodes e WHERE e.id=NEW.requested_episode_id AND json_extract(NEW.captured_target_json,'$.media_type')='tv' AND json_extract(NEW.captured_target_json,'$.episode_id')=e.id AND json_extract(NEW.captured_target_json,'$.series_id')=e.series_id)) OR (NEW.media_type='movies' AND EXISTS(SELECT 1 FROM movies m WHERE m.id=NEW.requested_movie_id AND json_extract(NEW.captured_target_json,'$.media_type')='movies' AND json_extract(NEW.captured_target_json,'$.movie_id')=m.id AND json_extract(NEW.captured_target_json,'$.metadata_id')=m.metadata_id))) THEN RAISE(ABORT,'invalid search target') END;
END;
CREATE TRIGGER search_commands_transition BEFORE UPDATE ON search_commands BEGIN
 SELECT CASE WHEN NEW.id IS NOT OLD.id OR NEW.mode IS NOT OLD.mode OR NEW.decision_context IS NOT OLD.decision_context OR NEW.requested_episode_id IS NOT OLD.requested_episode_id OR NEW.requested_movie_id IS NOT OLD.requested_movie_id OR NEW.captured_target_json IS NOT OLD.captured_target_json OR NEW.media_type IS NOT OLD.media_type OR NEW.indexer_id IS NOT OLD.indexer_id OR NEW.indexer_revision IS NOT OLD.indexer_revision OR NEW.client_id IS NOT OLD.client_id OR NEW.client_revision IS NOT OLD.client_revision OR NEW.priority IS NOT OLD.priority OR NEW.created_at IS NOT OLD.created_at THEN RAISE(ABORT,'command identity is immutable') END;
 SELECT CASE WHEN NOT ((OLD.status IN ('queued','retry_wait') AND NEW.status IN ('running','cancelled','failed')) OR (OLD.status='running' AND NEW.status IN ('running','retry_wait','succeeded','failed','cancelled'))) THEN RAISE(ABORT,'invalid command transition') END;
 SELECT CASE WHEN NEW.attempts != OLD.attempts + CASE WHEN NEW.status='running' AND OLD.status IN ('queued','retry_wait') THEN 1 ELSE 0 END THEN RAISE(ABORT,'invalid command attempt counter') END;
 SELECT CASE WHEN NEW.fetch_complete<OLD.fetch_complete OR (NEW.fetch_complete>OLD.fetch_complete AND NEW.status!='running') THEN RAISE(ABORT,'invalid search fetch checkpoint') END;
END;
DROP TRIGGER commands_admit;
CREATE TRIGGER commands_admit BEFORE INSERT ON commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands)+(SELECT count(*) FROM metadata_refresh_commands)+(SELECT count(*) FROM blocklist_clear_commands)+(SELECT count(*) FROM rss_commands)+(SELECT count(*) FROM search_commands)>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.provider_id AND p.revision=NEW.provider_revision AND p.enabled=1 AND p.implementation='qbittorrent' AND s.media_type=NEW.media_type) THEN RAISE(ABORT,'invalid refresh provider scope or revision') END;
END;
DROP TRIGGER metadata_refresh_admit;
CREATE TRIGGER metadata_refresh_admit BEFORE INSERT ON metadata_refresh_commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands)+(SELECT count(*) FROM metadata_refresh_commands)+(SELECT count(*) FROM blocklist_clear_commands)+(SELECT count(*) FROM rss_commands)+(SELECT count(*) FROM search_commands)>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM series s WHERE s.id=NEW.series_id AND s.tvdb_id=NEW.external_id) AND NOT EXISTS(SELECT 1 FROM movies m JOIN movie_metadata d ON d.id=m.metadata_id WHERE m.id=NEW.movie_id AND m.metadata_id=NEW.metadata_id AND d.tmdb_id=NEW.external_id) THEN RAISE(ABORT,'invalid metadata refresh target') END;
END;
DROP TRIGGER blocklist_clear_admit;
CREATE TRIGGER blocklist_clear_admit BEFORE INSERT ON blocklist_clear_commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands)+(SELECT count(*) FROM metadata_refresh_commands)+(SELECT count(*) FROM blocklist_clear_commands)+(SELECT count(*) FROM rss_commands)+(SELECT count(*) FROM search_commands)>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
END;
DROP TRIGGER rss_commands_admit;
CREATE TRIGGER rss_commands_admit BEFORE INSERT ON rss_commands BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands)+(SELECT count(*) FROM metadata_refresh_commands)+(SELECT count(*) FROM blocklist_clear_commands)+(SELECT count(*) FROM rss_commands)+(SELECT count(*) FROM search_commands)>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 OR NEW.fetch_complete!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT (EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.indexer_id AND p.revision=NEW.indexer_revision AND p.implementation IN ('torznab','newznab') AND s.media_type=NEW.media_type AND p.enabled=1) AND EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.client_id AND p.revision=NEW.client_revision AND p.implementation='qbittorrent' AND s.media_type=NEW.media_type AND p.enabled=1)) THEN RAISE(ABORT,'invalid RSS provider scope or revision') END;
END;
CREATE TABLE search_results (
 id TEXT PRIMARY KEY NOT NULL CHECK(length(id)=36 AND substr(id,9,1)='-' AND substr(id,14,1)='-' AND substr(id,19,1)='-' AND substr(id,24,1)='-' AND length(replace(id,'-',''))=32 AND replace(id,'-','') NOT GLOB '*[^0-9a-f]*'),
 command_id TEXT NOT NULL REFERENCES search_commands(id) ON DELETE RESTRICT,
 ordinal INTEGER NOT NULL CHECK(typeof(ordinal)='integer' AND ordinal BETWEEN 0 AND 999),
 fingerprint TEXT NOT NULL CHECK(length(fingerprint)=64 AND fingerprint NOT GLOB '*[^0-9a-f]*'),
 title TEXT NOT NULL CHECK(typeof(title)='text' AND length(CAST(title AS BLOB)) BETWEEN 1 AND 1024 AND instr(title,char(0))=0),
 metadata_json TEXT NOT NULL CHECK(length(CAST(metadata_json AS BLOB))<=65536 AND json_valid(metadata_json) AND json_type(metadata_json)='object'),
 decision_json TEXT NOT NULL CHECK(length(CAST(decision_json AS BLOB))<=16384 AND json_valid(decision_json) AND json_type(decision_json)='object'),
 private_payload BLOB CHECK(private_payload IS NULL OR (typeof(private_payload)='blob' AND length(private_payload) BETWEEN 29 AND 65565)),
 created_at INTEGER NOT NULL CHECK(typeof(created_at)='integer' AND created_at BETWEEN 0 AND 9007199254739191),
 expires_at INTEGER NOT NULL CHECK(typeof(expires_at)='integer' AND expires_at=created_at+1800),
 selected_candidate_id TEXT UNIQUE REFERENCES rss_candidates(id) ON DELETE RESTRICT,
 UNIQUE(command_id,ordinal), UNIQUE(command_id,fingerprint)
);
CREATE UNIQUE INDEX search_results_one_selection ON search_results(command_id) WHERE selected_candidate_id IS NOT NULL;
CREATE INDEX search_results_page ON search_results(command_id,ordinal,id);
CREATE INDEX search_results_expiry ON search_results(expires_at) WHERE private_payload IS NOT NULL AND selected_candidate_id IS NULL;
ALTER TABLE rss_candidates ADD COLUMN search_result_id TEXT REFERENCES search_results(id) ON DELETE RESTRICT;
CREATE UNIQUE INDEX candidate_search_result ON rss_candidates(search_result_id) WHERE search_result_id IS NOT NULL;
CREATE TRIGGER search_results_admit BEFORE INSERT ON search_results BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM search_results)>=1024 THEN RAISE(ABORT,'search result capacity reached') END;
 SELECT CASE WHEN (SELECT coalesce(sum(length(private_payload)),0) FROM search_results)+(SELECT coalesce(sum(length(private_payload)),0) FROM rss_candidates)+coalesce(length(NEW.private_payload),0)>16777216 THEN RAISE(ABORT,'release payload capacity reached') END;
 SELECT CASE WHEN NEW.selected_candidate_id IS NOT NULL OR NOT EXISTS(SELECT 1 FROM search_commands WHERE id=NEW.command_id AND status='running' AND fetch_complete=0) THEN RAISE(ABORT,'search results require running fetch') END;
END;
CREATE TRIGGER search_results_immutable BEFORE UPDATE ON search_results BEGIN
 SELECT CASE WHEN NEW.id IS NOT OLD.id OR NEW.command_id IS NOT OLD.command_id OR NEW.ordinal IS NOT OLD.ordinal OR NEW.fingerprint IS NOT OLD.fingerprint OR NEW.title IS NOT OLD.title OR NEW.metadata_json IS NOT OLD.metadata_json OR NEW.decision_json IS NOT OLD.decision_json OR NEW.created_at IS NOT OLD.created_at OR NEW.expires_at IS NOT OLD.expires_at OR OLD.selected_candidate_id IS NOT NULL THEN RAISE(ABORT,'search result facts are immutable') END;
 SELECT CASE WHEN NEW.private_payload IS NOT NULL OR NOT ((NEW.selected_candidate_id IS NULL AND OLD.expires_at<=unixepoch()) OR (NEW.selected_candidate_id IS NOT NULL AND EXISTS(SELECT 1 FROM rss_candidates r WHERE r.id=NEW.selected_candidate_id AND r.search_result_id=OLD.id AND r.command_id IS NULL AND r.status='pending'))) THEN RAISE(ABORT,'search payload can only expire or transfer once') END;
END;
CREATE TRIGGER search_results_delete BEFORE DELETE ON search_results
WHEN OLD.selected_candidate_id IS NOT NULL OR EXISTS(SELECT 1 FROM search_commands WHERE id=OLD.command_id AND status IN ('queued','running','retry_wait'))
BEGIN SELECT RAISE(ABORT,'selected or active search results must be retained'); END;
CREATE TRIGGER search_commands_delete BEFORE DELETE ON search_commands
WHEN OLD.status IN ('queued','running','retry_wait') OR EXISTS(SELECT 1 FROM search_results WHERE command_id=OLD.id AND selected_candidate_id IS NOT NULL)
BEGIN SELECT RAISE(ABORT,'selected or active search command must be retained'); END;
DROP TRIGGER rss_candidate_admit;
CREATE TRIGGER rss_candidate_admit BEFORE INSERT ON rss_candidates BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM rss_candidates)>=1024 THEN RAISE(ABORT,'RSS candidate capacity reached') END;
 -- A selected offer is transferred by the mandatory AFTER trigger in this same statement.
 SELECT CASE WHEN (SELECT coalesce(sum(length(private_payload)),0) FROM rss_candidates)+(SELECT coalesce(sum(length(private_payload)),0) FROM search_results)+coalesce(length(NEW.private_payload),0)-coalesce((SELECT length(private_payload) FROM search_results WHERE id=NEW.search_result_id AND selected_candidate_id IS NULL),0)>16777216 THEN RAISE(ABORT,'release payload capacity reached') END;
 SELECT CASE WHEN NEW.status NOT IN ('pending','rejected') OR NEW.attempts!=0 OR NEW.submission_identity_json IS NOT NULL THEN RAISE(ABORT,'invalid initial RSS candidate state') END;
 SELECT CASE WHEN NEW.search_result_id IS NULL AND NOT EXISTS(SELECT 1 FROM rss_commands c WHERE c.id=NEW.command_id AND c.media_type=NEW.media_type AND c.indexer_id=NEW.indexer_id AND c.indexer_revision=NEW.indexer_revision AND c.client_id=NEW.client_id AND c.client_revision=NEW.client_revision) THEN RAISE(ABORT,'candidate requires matching RSS command') END;
 SELECT CASE WHEN NEW.search_result_id IS NOT NULL AND (NEW.command_id IS NOT NULL OR NEW.status!='pending' OR NOT EXISTS(SELECT 1 FROM search_results o JOIN search_commands c ON c.id=o.command_id WHERE o.id=NEW.search_result_id AND o.selected_candidate_id IS NULL AND o.private_payload IS NOT NULL AND o.expires_at>unixepoch() AND c.fetch_complete=1 AND ((c.mode='automatic' AND c.status='running') OR (c.mode='interactive' AND c.status='succeeded')) AND c.media_type=NEW.media_type AND c.indexer_id=NEW.indexer_id AND c.indexer_revision=NEW.indexer_revision AND c.client_id=NEW.client_id AND c.client_revision=NEW.client_revision AND o.fingerprint=NEW.fingerprint AND o.title=NEW.title AND ((c.media_type='movies' AND NEW.movie_id=c.requested_movie_id) OR (c.media_type='tv' AND NEW.series_id=json_extract(c.captured_target_json,'$.series_id'))))) THEN RAISE(ABORT,'candidate requires selectable captured search result') END;
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
CREATE TRIGGER search_candidate_episode BEFORE INSERT ON rss_candidate_episodes
WHEN EXISTS(SELECT 1 FROM rss_candidates r JOIN search_results o ON o.id=r.search_result_id JOIN search_commands c ON c.id=o.command_id WHERE r.id=NEW.candidate_id AND NEW.episode_id IS NOT c.requested_episode_id)
BEGIN SELECT RAISE(ABORT,'episode differs from requested search target'); END;
