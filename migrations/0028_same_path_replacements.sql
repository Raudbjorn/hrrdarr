-- Existing v1 plans/stages keep their meaning; this relational protocol owns exchanges.
CREATE TABLE same_path_replacements (
 operation_id TEXT PRIMARY KEY NOT NULL REFERENCES import_journal(operation_id) ON DELETE RESTRICT,
 protocol_version INTEGER NOT NULL DEFAULT 1 CHECK(typeof(protocol_version)='integer' AND protocol_version=1),
 state TEXT NOT NULL DEFAULT 'exchange_intent' CHECK(state IN ('exchange_intent','installed','restore_intent','restored'))
);
CREATE TRIGGER same_path_admit BEFORE INSERT ON same_path_replacements BEGIN
 SELECT CASE WHEN NEW.state!='exchange_intent' OR NOT EXISTS(
 SELECT 1 FROM import_journal j JOIN operations o ON o.id=j.operation_id JOIN rss_candidate_imports i ON i.operation_id=j.operation_id
 WHERE j.operation_id=NEW.operation_id AND j.version=1 AND j.phase='staged'
 AND json_type(j.stage_json,'$.sha256')='text' AND length(json_extract(j.stage_json,'$.sha256'))=64 AND json_extract(j.stage_json,'$.sha256') NOT GLOB '*[^0-9a-f]*'
 AND o.destination=json_extract(j.plan_json,'$.destination') AND o.destination=json_extract(i.old_file_json,'$.path')
 AND i.retirement_state='pending' AND i.retirement_json IS NULL
 AND ((o.media_type='episode' AND EXISTS(SELECT 1 FROM episodes e JOIN episode_files f ON f.id=e.episode_file_id WHERE e.id=o.episode_id AND f.id=i.old_episode_file_id AND f.path=o.destination) AND (SELECT count(*) FROM episodes WHERE episode_file_id=i.old_episode_file_id)=1)
 OR (o.media_type='movie' AND EXISTS(SELECT 1 FROM movie_files f WHERE f.movie_id=o.movie_id AND f.id=i.old_movie_file_id AND f.path=o.destination))))
 THEN RAISE(ABORT,'same path exchange requires staged owned unshared original') END;
END;
CREATE TRIGGER same_path_transition BEFORE UPDATE ON same_path_replacements BEGIN
 SELECT CASE WHEN NEW.operation_id IS NOT OLD.operation_id OR NEW.protocol_version IS NOT OLD.protocol_version THEN RAISE(ABORT,'same path identity is immutable') END;
 SELECT CASE WHEN NEW.state IS NOT OLD.state AND (EXISTS(SELECT 1 FROM import_journal WHERE operation_id=OLD.operation_id AND phase IN ('committed','complete')) OR EXISTS(SELECT 1 FROM import_history WHERE operation_id=OLD.operation_id)) THEN RAISE(ABORT,'committed replacement cannot restore') END;
 SELECT CASE WHEN NOT (NEW.state=OLD.state OR (OLD.state='exchange_intent' AND NEW.state IN ('installed','restore_intent')) OR (OLD.state='installed' AND NEW.state='restore_intent') OR (OLD.state='restore_intent' AND NEW.state='restored') OR (OLD.state='restored' AND NEW.state='exchange_intent')) THEN RAISE(ABORT,'invalid same path transition') END;
END;
CREATE TRIGGER same_path_retained BEFORE DELETE ON same_path_replacements BEGIN SELECT RAISE(ABORT,'same path authority must be retained'); END;
CREATE TRIGGER same_path_commit BEFORE UPDATE OF phase ON import_journal
WHEN NEW.phase IN ('committed','complete') AND EXISTS(SELECT 1 FROM rss_candidate_imports i WHERE i.operation_id=NEW.operation_id AND json_extract(i.old_file_json,'$.path')=json_extract(NEW.plan_json,'$.destination')) AND NOT EXISTS(SELECT 1 FROM same_path_replacements WHERE operation_id=NEW.operation_id AND state='installed')
BEGIN SELECT RAISE(ABORT,'same path commit requires installed replacement'); END;
DROP TRIGGER replaced_episode_file_path;
CREATE TRIGGER replaced_episode_file_path BEFORE UPDATE OF path ON episode_files
WHEN NEW.path IS NOT OLD.path AND EXISTS(SELECT 1 FROM rss_candidate_imports WHERE old_episode_file_id=OLD.id)
 AND NOT EXISTS(SELECT 1 FROM rss_candidate_imports i JOIN import_journal j ON j.operation_id=i.operation_id WHERE i.old_episode_file_id=OLD.id AND i.retirement_state='pending' AND NOT EXISTS(SELECT 1 FROM episodes WHERE episode_file_id=OLD.id)
 AND (j.phase='committed' OR (j.phase='published' AND EXISTS(SELECT 1 FROM same_path_replacements x WHERE x.operation_id=j.operation_id AND x.state='installed') AND NEW.path=rtrim(json_extract(i.old_file_json,'$.path'),replace(json_extract(i.old_file_json,'$.path'),'/',''))||json_extract(i.old_file_json,'$.quarantine_name')||'/original')))
BEGIN SELECT RAISE(ABORT,'replaced episode path requires retirement checkpoint'); END;
DROP TRIGGER owned_old_path_episode_insert;
CREATE TRIGGER owned_old_path_episode_insert BEFORE INSERT ON episode_files
WHEN EXISTS(SELECT 1 FROM rss_candidate_imports i JOIN import_journal j ON j.operation_id=i.operation_id WHERE j.phase!='complete' AND json_extract(i.old_file_json,'$.path')=NEW.path
 AND NOT EXISTS(SELECT 1 FROM same_path_replacements x JOIN operations o ON o.id=x.operation_id JOIN episodes e ON e.id=o.episode_id JOIN episode_files old ON old.id=i.old_episode_file_id
 WHERE x.operation_id=i.operation_id AND x.state='installed' AND j.phase='published' AND o.media_type='episode' AND e.series_id=NEW.series_id AND e.episode_file_id IS NULL
 AND NOT EXISTS(SELECT 1 FROM episodes WHERE episode_file_id=old.id)
 AND old.path=rtrim(json_extract(i.old_file_json,'$.path'),replace(json_extract(i.old_file_json,'$.path'),'/',''))||json_extract(i.old_file_json,'$.quarantine_name')||'/original'))
BEGIN SELECT RAISE(ABORT,'file path is owned by unfinished download import'); END;
