-- Completed downloads are authorized per concrete client/domain. Absence means disabled.
CREATE TABLE download_processing_policies (
 provider_id TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
 media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
 provider_revision INTEGER NOT NULL CHECK(typeof(provider_revision)='integer' AND provider_revision BETWEEN 1 AND 9007199254740991),
 revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision BETWEEN 1 AND 9007199254740991),
 enabled INTEGER NOT NULL CHECK(typeof(enabled)='integer' AND enabled IN (0,1)),
 mode TEXT NOT NULL CHECK(mode IN ('copy','hardlink')),
 PRIMARY KEY(provider_id,media_type)
);
CREATE TRIGGER processing_policy_insert BEFORE INSERT ON download_processing_policies BEGIN
 SELECT CASE WHEN NEW.revision!=1 OR NOT EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.provider_id AND p.revision=NEW.provider_revision AND p.implementation='qbittorrent' AND s.media_type=NEW.media_type AND (NEW.enabled=0 OR p.enabled=1)) THEN RAISE(ABORT,'invalid processing policy') END;
END;
CREATE TRIGGER processing_policy_update BEFORE UPDATE ON download_processing_policies BEGIN
 SELECT CASE WHEN NEW.provider_id IS NOT OLD.provider_id OR NEW.media_type IS NOT OLD.media_type OR NEW.revision!=OLD.revision+1 THEN RAISE(ABORT,'invalid processing policy revision') END;
 SELECT CASE WHEN (NEW.enabled=1 OR NEW.provider_revision IS NOT OLD.provider_revision) AND NOT EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.provider_id AND p.revision=NEW.provider_revision AND p.implementation='qbittorrent' AND s.media_type=NEW.media_type AND (NEW.enabled=0 OR p.enabled=1)) THEN RAISE(ABORT,'stale processing policy') END;
END;
CREATE TRIGGER processing_provider_changed AFTER UPDATE ON providers BEGIN
 UPDATE download_processing_policies SET enabled=0,revision=revision+1 WHERE provider_id=OLD.id;
END;
CREATE TRIGGER processing_scope_insert AFTER INSERT ON provider_scopes BEGIN
 UPDATE download_processing_policies SET enabled=0,revision=revision+1 WHERE provider_id=NEW.provider_id AND media_type=NEW.media_type;
END;
CREATE TRIGGER processing_scope_update AFTER UPDATE ON provider_scopes BEGIN
 UPDATE download_processing_policies SET enabled=0,revision=revision+1 WHERE (provider_id=OLD.provider_id AND media_type=OLD.media_type) OR (provider_id=NEW.provider_id AND media_type=NEW.media_type);
END;
CREATE TRIGGER processing_scope_delete AFTER DELETE ON provider_scopes BEGIN
 UPDATE download_processing_policies SET enabled=0,revision=revision+1 WHERE provider_id=OLD.provider_id AND media_type=OLD.media_type;
END;

-- The operation and receipt retain their original typed identity. No new submission authority.
CREATE TABLE rss_candidate_imports (
 candidate_id TEXT PRIMARY KEY NOT NULL REFERENCES rss_candidates(id) ON DELETE RESTRICT,
 operation_id TEXT NOT NULL UNIQUE REFERENCES import_journal(operation_id) ON DELETE RESTRICT,
 quality_id INTEGER NOT NULL CHECK(typeof(quality_id)='integer'),
 revision_json TEXT NOT NULL CHECK(length(CAST(revision_json AS BLOB))<=1024 AND json_valid(revision_json) AND json_type(revision_json)='object'),
 edition TEXT CHECK(edition IS NULL OR (typeof(edition)='text' AND length(CAST(edition AS BLOB)) BETWEEN 1 AND 1024)),
 provenance_json TEXT NOT NULL CHECK(length(CAST(provenance_json AS BLOB))<=16384 AND json_valid(provenance_json) AND json_type(provenance_json)='object'),
 old_episode_file_id INTEGER REFERENCES episode_files(id) ON DELETE RESTRICT,
 old_movie_file_id INTEGER REFERENCES movie_files(id) ON DELETE RESTRICT,
 old_file_json TEXT CHECK(old_file_json IS NULL OR (length(CAST(old_file_json AS BLOB))<=131072 AND json_valid(old_file_json) AND json_type(old_file_json)='object')),
 retirement_state TEXT NOT NULL DEFAULT 'pending' CHECK(retirement_state IN ('pending','quarantined','shared_retained')),
 retirement_json TEXT CHECK(retirement_json IS NULL OR (length(CAST(retirement_json AS BLOB))<=4096 AND json_valid(retirement_json) AND json_type(retirement_json)='object')),
 CHECK((old_file_json IS NULL AND old_episode_file_id IS NULL AND old_movie_file_id IS NULL) OR (old_file_json IS NOT NULL AND ((old_episode_file_id IS NOT NULL AND old_movie_file_id IS NULL) OR (old_episode_file_id IS NULL AND old_movie_file_id IS NOT NULL)))),
 CHECK(retirement_state!='quarantined' OR retirement_json IS NOT NULL),
 CHECK(retirement_json IS NULL OR json_type(retirement_json,'$.directory') IS 'object')
);
CREATE INDEX rss_retired_episode_file ON rss_candidate_imports(old_episode_file_id,retirement_state);
CREATE TRIGGER candidate_import_admit BEFORE INSERT ON rss_candidate_imports BEGIN
 SELECT CASE WHEN NEW.retirement_state!='pending' OR NEW.retirement_json IS NOT NULL THEN RAISE(ABORT,'new import cannot claim retirement') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM rss_candidates r JOIN operations o ON o.id=NEW.operation_id JOIN import_journal j ON j.operation_id=o.id JOIN quality_definitions q ON q.media_type=r.media_type AND q.quality_id=NEW.quality_id WHERE r.id=NEW.candidate_id AND r.status='observed' AND j.phase='preview' AND ((r.media_type='movies' AND o.media_type='movie' AND o.movie_id=r.movie_id AND NEW.old_episode_file_id IS NULL) OR (r.media_type='tv' AND o.media_type='episode' AND NEW.old_movie_file_id IS NULL AND NEW.edition IS NULL AND EXISTS(SELECT 1 FROM rss_candidate_episodes e WHERE e.candidate_id=r.id AND e.episode_id=o.episode_id)))) THEN RAISE(ABORT,'import requires matching observed receipt and quality') END;
 SELECT CASE WHEN json_type(NEW.revision_json,'$.version') IS NOT 'integer' OR json_extract(NEW.revision_json,'$.version')<1 OR json_type(NEW.revision_json,'$.real') IS NOT 'integer' OR json_extract(NEW.revision_json,'$.real')<0 OR (json_type(NEW.revision_json,'$.is_repack') IS NOT 'true' AND json_type(NEW.revision_json,'$.is_repack') IS NOT 'false') OR (SELECT count(*) FROM json_each(NEW.revision_json))!=3 THEN RAISE(ABORT,'invalid import quality revision') END;
 SELECT CASE WHEN NEW.old_file_json IS NOT NULL AND (json_extract(NEW.old_file_json,'$.version') IS NOT 1 OR json_extract(NEW.old_file_json,'$.file_id') IS NOT coalesce(NEW.old_episode_file_id,NEW.old_movie_file_id) OR json_type(NEW.old_file_json,'$.path') IS NOT 'text' OR length(CAST(json_extract(NEW.old_file_json,'$.path') AS BLOB)) NOT BETWEEN 1 AND 4096 OR json_extract(NEW.old_file_json,'$.quarantine_name') IS NOT '.hrrdarr-replaced-'||NEW.operation_id) THEN RAISE(ABORT,'invalid replaced file archive') END;
 SELECT CASE WHEN NEW.old_file_json IS NULL AND EXISTS(SELECT 1 FROM operations o WHERE o.id=NEW.operation_id AND ((o.media_type='episode' AND EXISTS(SELECT 1 FROM episodes e WHERE e.id=o.episode_id AND e.episode_file_id IS NOT NULL)) OR (o.media_type='movie' AND EXISTS(SELECT 1 FROM movie_files f WHERE f.movie_id=o.movie_id)))) THEN RAISE(ABORT,'replacement archive is required') END;
 SELECT CASE WHEN NEW.old_episode_file_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM operations o JOIN episodes e ON e.id=o.episode_id JOIN episode_files f ON f.id=e.episode_file_id WHERE o.id=NEW.operation_id AND f.id=NEW.old_episode_file_id AND f.path=json_extract(NEW.old_file_json,'$.path')) THEN RAISE(ABORT,'replaced episode file changed') END;
 SELECT CASE WHEN NEW.old_movie_file_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM operations o JOIN movie_files f ON f.movie_id=o.movie_id WHERE o.id=NEW.operation_id AND f.id=NEW.old_movie_file_id AND f.path=json_extract(NEW.old_file_json,'$.path')) THEN RAISE(ABORT,'replaced movie file changed') END;
END;
CREATE TRIGGER candidate_import_immutable BEFORE UPDATE ON rss_candidate_imports BEGIN
 SELECT CASE WHEN NEW.candidate_id IS NOT OLD.candidate_id OR NEW.operation_id IS NOT OLD.operation_id OR NEW.quality_id IS NOT OLD.quality_id OR NEW.revision_json IS NOT OLD.revision_json OR NEW.edition IS NOT OLD.edition OR NEW.provenance_json IS NOT OLD.provenance_json OR NEW.old_episode_file_id IS NOT OLD.old_episode_file_id OR NEW.old_movie_file_id IS NOT OLD.old_movie_file_id OR NEW.old_file_json IS NOT OLD.old_file_json THEN RAISE(ABORT,'download import facts are immutable') END;
 SELECT CASE WHEN OLD.old_file_json IS NULL OR OLD.retirement_state!='pending' OR NOT EXISTS(SELECT 1 FROM import_journal WHERE operation_id=OLD.operation_id AND phase='committed') THEN RAISE(ABORT,'retirement requires committed import') END;
 SELECT CASE WHEN OLD.retirement_json IS NOT NULL AND json_extract(NEW.retirement_json,'$.directory.dev','$.directory.ino','$.directory.size','$.directory.mtime','$.directory.mtime_ns','$.directory.ctime','$.directory.ctime_ns') IS NOT json_extract(OLD.retirement_json,'$.directory.dev','$.directory.ino','$.directory.size','$.directory.mtime','$.directory.mtime_ns','$.directory.ctime','$.directory.ctime_ns') THEN RAISE(ABORT,'retirement directory is immutable') END;
 SELECT CASE WHEN NEW.retirement_state='shared_retained' AND (NEW.old_episode_file_id IS NULL OR NOT EXISTS(SELECT 1 FROM episodes WHERE episode_file_id=NEW.old_episode_file_id)) THEN RAISE(ABORT,'shared retention requires live episode association') END;
 SELECT CASE WHEN NEW.retirement_state='quarantined' AND NEW.old_episode_file_id IS NOT NULL AND (EXISTS(SELECT 1 FROM episodes WHERE episode_file_id=NEW.old_episode_file_id) OR NOT EXISTS(SELECT 1 FROM episode_files f WHERE f.id=NEW.old_episode_file_id AND f.path!=json_extract(NEW.old_file_json,'$.path'))) THEN RAISE(ABORT,'quarantine requires retired episode path') END;
END;
CREATE TRIGGER candidate_import_retained BEFORE DELETE ON rss_candidate_imports BEGIN SELECT RAISE(ABORT,'download import facts must be retained'); END;
CREATE TRIGGER import_complete_requires_retirement BEFORE UPDATE OF phase ON import_journal
WHEN NEW.phase='complete' AND EXISTS(SELECT 1 FROM rss_candidate_imports i WHERE i.operation_id=NEW.operation_id AND i.old_file_json IS NOT NULL AND i.retirement_state NOT IN ('quarantined','shared_retained'))
BEGIN SELECT RAISE(ABORT,'replacement retirement must complete'); END;

-- Release only the target claim after completed import; hash/remote identity claims remain.
DROP INDEX rss_movie_target_owned;
DROP TRIGGER rss_episode_target_owned;
CREATE TRIGGER rss_movie_target_owned BEFORE UPDATE OF status ON rss_candidates
WHEN NEW.status='prepared' AND EXISTS(SELECT 1 FROM rss_candidates owner WHERE owner.id!=NEW.id AND owner.movie_id=NEW.movie_id AND owner.status IN ('prepared','submitting','reconciling','observed','needs_attention') AND NOT EXISTS(SELECT 1 FROM rss_candidate_imports i JOIN import_journal j ON j.operation_id=i.operation_id JOIN import_history h ON h.operation_id=j.operation_id WHERE i.candidate_id=owner.id AND j.phase='complete'))
BEGIN SELECT RAISE(ABORT,'movie target already owned'); END;
CREATE TRIGGER rss_episode_target_owned BEFORE UPDATE OF status ON rss_candidates
WHEN NEW.status='prepared' AND EXISTS(SELECT 1 FROM rss_candidate_episodes incoming JOIN rss_candidate_episodes existing ON existing.episode_id=incoming.episode_id JOIN rss_candidates owner ON owner.id=existing.candidate_id WHERE incoming.candidate_id=NEW.id AND owner.id!=NEW.id AND owner.status IN ('prepared','submitting','reconciling','observed','needs_attention') AND NOT EXISTS(SELECT 1 FROM rss_candidate_imports i JOIN import_journal j ON j.operation_id=i.operation_id JOIN import_history h ON h.operation_id=j.operation_id WHERE i.candidate_id=owner.id AND j.phase='complete'))
BEGIN SELECT RAISE(ABORT,'episode target already owned'); END;

CREATE TABLE download_processing (
 candidate_id TEXT PRIMARY KEY NOT NULL REFERENCES rss_candidates(id) ON DELETE RESTRICT,
 policy_revision INTEGER NOT NULL CHECK(typeof(policy_revision)='integer' AND policy_revision BETWEEN 1 AND 9007199254740991),
 status TEXT NOT NULL CHECK(status IN ('queued','checking','importing','imported','blocked','cancelled')),
 resume_requested INTEGER NOT NULL DEFAULT 0 CHECK(typeof(resume_requested)='integer' AND resume_requested IN (0,1)),
 preflight_attempts INTEGER NOT NULL DEFAULT 0 CHECK(typeof(preflight_attempts)='integer' AND preflight_attempts BETWEEN 0 AND 3),
 total_preflight_attempts INTEGER NOT NULL DEFAULT 0 CHECK(typeof(total_preflight_attempts)='integer' AND total_preflight_attempts BETWEEN 0 AND 9007199254740991),
 next_attempt_at INTEGER NOT NULL CHECK(typeof(next_attempt_at)='integer' AND next_attempt_at BETWEEN 0 AND 9007199254740991),
 created_at INTEGER NOT NULL CHECK(typeof(created_at)='integer' AND created_at BETWEEN 0 AND 9007199254740991),
 updated_at INTEGER NOT NULL CHECK(typeof(updated_at)='integer' AND updated_at BETWEEN 0 AND 9007199254740991),
 error_code TEXT CHECK(error_code IS NULL OR error_code IN ('interrupted','storage_error','provider_changed','processing_disabled','download_not_complete','download_failed','download_unavailable','download_timeout','path_mapping_missing','path_mapping_changed','invalid_download_files','ambiguous_files','source_unavailable','target_changed','unsupported_download','quality_rejected','import_busy','import_failed','import_conflict')),
 reasons_json TEXT NOT NULL DEFAULT '[]' CHECK(length(CAST(reasons_json AS BLOB))<=8192 AND json_valid(reasons_json) AND json_type(reasons_json)='array' AND json_array_length(reasons_json)<=64),
 CHECK(status='importing' OR resume_requested=0),
 CHECK(total_preflight_attempts>=preflight_attempts),
 CHECK(status!='queued' OR preflight_attempts<3),
 CHECK(status NOT IN ('checking','importing','imported') OR preflight_attempts>0),
 CHECK(status!='imported' OR error_code IS NULL),
 CHECK(status!='blocked' OR error_code IS NOT NULL)
);
CREATE INDEX download_processing_ready ON download_processing(status,next_attempt_at,created_at,candidate_id);
CREATE TRIGGER processing_admit BEFORE INSERT ON download_processing BEGIN
 SELECT CASE WHEN NEW.status!='queued' OR NEW.preflight_attempts!=0 OR NEW.total_preflight_attempts!=0 OR NEW.resume_requested!=0 THEN RAISE(ABORT,'processing must be queued') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM rss_candidates r JOIN download_processing_policies p ON p.provider_id=r.client_id AND p.media_type=r.media_type JOIN providers v ON v.id=p.provider_id JOIN provider_scopes s ON s.provider_id=v.id AND s.media_type=r.media_type WHERE r.id=NEW.candidate_id AND r.status='observed' AND p.enabled=1 AND v.enabled=1 AND v.implementation='qbittorrent' AND p.revision=NEW.policy_revision AND p.provider_revision=r.client_revision AND v.revision=r.client_revision) THEN RAISE(ABORT,'processing requires current authorized observed receipt') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM json_each(NEW.reasons_json) WHERE type!='text' OR length(value)>128 OR value GLOB '*[^a-z0-9_]*') THEN RAISE(ABORT,'invalid processing reason') END;
END;
CREATE TRIGGER processing_transition BEFORE UPDATE ON download_processing BEGIN
 SELECT CASE WHEN NEW.candidate_id IS NOT OLD.candidate_id OR NEW.created_at IS NOT OLD.created_at THEN RAISE(ABORT,'processing identity is immutable') END;
 SELECT CASE WHEN NOT ((OLD.status='queued' AND NEW.status IN ('checking','blocked','cancelled')) OR (OLD.status='checking' AND NEW.status IN ('queued','importing','blocked','cancelled')) OR (OLD.status='importing' AND NEW.status IN ('importing','imported')) OR (OLD.status IN ('blocked','cancelled') AND NEW.status IN ('queued','cancelled'))) THEN RAISE(ABORT,'invalid processing transition') END;
 SELECT CASE WHEN NEW.total_preflight_attempts!=OLD.total_preflight_attempts+CASE WHEN OLD.status='queued' AND NEW.status='checking' THEN 1 ELSE 0 END OR NEW.preflight_attempts!=CASE WHEN OLD.status='queued' AND NEW.status='checking' THEN OLD.preflight_attempts+1 WHEN OLD.status IN ('blocked','cancelled') AND NEW.status='queued' THEN 0 ELSE OLD.preflight_attempts END THEN RAISE(ABORT,'invalid processing attempt counter') END;
 SELECT CASE WHEN NEW.policy_revision IS NOT OLD.policy_revision AND NOT(OLD.status IN ('blocked','cancelled') AND NEW.status='queued') THEN RAISE(ABORT,'processing authorization is immutable within retry round') END;
 SELECT CASE WHEN NEW.status IN ('checking','queued') AND NOT EXISTS(SELECT 1 FROM rss_candidates r JOIN download_processing_policies p ON p.provider_id=r.client_id AND p.media_type=r.media_type JOIN providers v ON v.id=p.provider_id JOIN provider_scopes s ON s.provider_id=v.id AND s.media_type=r.media_type WHERE r.id=NEW.candidate_id AND r.status='observed' AND p.enabled=1 AND v.enabled=1 AND p.revision=NEW.policy_revision AND p.provider_revision=r.client_revision AND v.revision=r.client_revision) THEN RAISE(ABORT,'processing authorization changed') END;
 SELECT CASE WHEN NEW.status IN ('queued','checking','blocked','cancelled') AND EXISTS(SELECT 1 FROM rss_candidate_imports WHERE candidate_id=NEW.candidate_id) THEN RAISE(ABORT,'linked imports must resume same operation') END;
 SELECT CASE WHEN NEW.status IN ('importing','imported') AND NOT EXISTS(SELECT 1 FROM rss_candidate_imports WHERE candidate_id=NEW.candidate_id) THEN RAISE(ABORT,'processing requires linked import') END;
 SELECT CASE WHEN NEW.status='imported' AND NOT EXISTS(SELECT 1 FROM rss_candidate_imports i JOIN import_journal j ON j.operation_id=i.operation_id JOIN import_history h ON h.operation_id=j.operation_id WHERE i.candidate_id=NEW.candidate_id AND j.phase='complete') THEN RAISE(ABORT,'processing requires complete import history') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM json_each(NEW.reasons_json) WHERE type!='text' OR length(value)>128 OR value GLOB '*[^a-z0-9_]*') THEN RAISE(ABORT,'invalid processing reason') END;
END;
CREATE TRIGGER processing_retained BEFORE DELETE ON download_processing BEGIN SELECT RAISE(ABORT,'download processing journal must be retained'); END;
-- Existing shared links may remain or detach, but replacement freezes new acquisitions.
CREATE TRIGGER replaced_episode_file_no_attach BEFORE INSERT ON episodes
WHEN NEW.episode_file_id IS NOT NULL AND EXISTS(SELECT 1 FROM rss_candidate_imports WHERE old_episode_file_id=NEW.episode_file_id)
BEGIN SELECT RAISE(ABORT,'replaced episode file cannot acquire associations'); END;
CREATE TRIGGER replaced_episode_file_no_reattach BEFORE UPDATE OF episode_file_id ON episodes
WHEN NEW.episode_file_id IS NOT OLD.episode_file_id AND NEW.episode_file_id IS NOT NULL AND EXISTS(SELECT 1 FROM rss_candidate_imports WHERE old_episode_file_id=NEW.episode_file_id)
BEGIN SELECT RAISE(ABORT,'replaced episode file cannot acquire associations'); END;
CREATE TRIGGER replaced_episode_file_path BEFORE UPDATE OF path ON episode_files
WHEN NEW.path IS NOT OLD.path AND EXISTS(SELECT 1 FROM rss_candidate_imports WHERE old_episode_file_id=OLD.id)
 AND NOT EXISTS(SELECT 1 FROM rss_candidate_imports i JOIN import_journal j ON j.operation_id=i.operation_id WHERE i.old_episode_file_id=OLD.id AND i.retirement_state='pending' AND j.phase='committed' AND NOT EXISTS(SELECT 1 FROM episodes WHERE episode_file_id=OLD.id))
BEGIN SELECT RAISE(ABORT,'replaced episode path requires retirement checkpoint'); END;
-- Filesystem work runs outside transactions; linked ownership pins its library root until done.
CREATE TRIGGER owned_import_series_root BEFORE UPDATE OF path ON series
WHEN NEW.path IS NOT OLD.path AND EXISTS(SELECT 1 FROM rss_candidate_imports i JOIN import_journal j ON j.operation_id=i.operation_id JOIN operations o ON o.id=i.operation_id JOIN episodes e ON e.id=o.episode_id WHERE e.series_id=OLD.id AND j.phase!='complete')
BEGIN SELECT RAISE(ABORT,'library root is owned by unfinished download import'); END;
CREATE TRIGGER owned_import_movie_root BEFORE UPDATE OF path ON movies
WHEN NEW.path IS NOT OLD.path AND EXISTS(SELECT 1 FROM rss_candidate_imports i JOIN import_journal j ON j.operation_id=i.operation_id JOIN operations o ON o.id=i.operation_id WHERE o.movie_id=OLD.id AND j.phase!='complete')
BEGIN SELECT RAISE(ABORT,'library root is owned by unfinished download import'); END;
-- A captured old path is a filesystem claim across both domains until retirement completes.
CREATE TRIGGER owned_old_path_episode_insert BEFORE INSERT ON episode_files
WHEN EXISTS(SELECT 1 FROM rss_candidate_imports i JOIN import_journal j ON j.operation_id=i.operation_id WHERE j.phase!='complete' AND json_extract(i.old_file_json,'$.path')=NEW.path)
BEGIN SELECT RAISE(ABORT,'file path is owned by unfinished download import'); END;
CREATE TRIGGER owned_old_path_episode_update BEFORE UPDATE OF path ON episode_files
WHEN NEW.path IS NOT OLD.path AND EXISTS(SELECT 1 FROM rss_candidate_imports i JOIN import_journal j ON j.operation_id=i.operation_id WHERE j.phase!='complete' AND json_extract(i.old_file_json,'$.path')=NEW.path)
BEGIN SELECT RAISE(ABORT,'file path is owned by unfinished download import'); END;
CREATE TRIGGER owned_old_path_movie_insert BEFORE INSERT ON movie_files
WHEN EXISTS(SELECT 1 FROM rss_candidate_imports i JOIN import_journal j ON j.operation_id=i.operation_id WHERE j.phase!='complete' AND json_extract(i.old_file_json,'$.path')=NEW.path)
BEGIN SELECT RAISE(ABORT,'file path is owned by unfinished download import'); END;
CREATE TRIGGER owned_old_path_movie_update BEFORE UPDATE OF path ON movie_files
WHEN NEW.path IS NOT OLD.path AND EXISTS(SELECT 1 FROM rss_candidate_imports i JOIN import_journal j ON j.operation_id=i.operation_id WHERE j.phase!='complete' AND json_extract(i.old_file_json,'$.path')=NEW.path)
BEGIN SELECT RAISE(ABORT,'file path is owned by unfinished download import'); END;
CREATE TRIGGER candidate_import_old_path_unique BEFORE INSERT ON rss_candidate_imports
WHEN NEW.old_file_json IS NOT NULL AND ((SELECT count(*) FROM episode_files WHERE path=json_extract(NEW.old_file_json,'$.path'))+(SELECT count(*) FROM movie_files WHERE path=json_extract(NEW.old_file_json,'$.path')))!=1
BEGIN SELECT RAISE(ABORT,'replaced path has ambiguous file ownership'); END;
