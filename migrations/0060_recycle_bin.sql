-- Recycle bin (Sonarr/Radarr media management) and explicit file deletion, both domains.
-- Settings never authorize deletion by themselves: every removal is an explicit API request
-- whose intent is journaled before any filesystem effect, and the journal is retained.
CREATE TABLE recycle_bin_settings (
 media_type TEXT NOT NULL PRIMARY KEY CHECK(media_type IN ('tv','movies')),
 path TEXT CHECK(path IS NULL OR (typeof(path)='text' AND length(CAST(path AS BLOB)) BETWEEN 2 AND 4096 AND substr(path,1,1)='/' AND substr(path,-1)!='/' AND instr(path,'//')=0 AND instr(path,char(0))=0 AND path NOT GLOB ('*['||char(1)||'-'||char(31)||char(127)||']*') AND instr(path,char(92))=0 AND instr(path||'/','/../')=0 AND instr(path||'/','/./')=0)),
 cleanup_days INTEGER NOT NULL DEFAULT 7 CHECK(typeof(cleanup_days)='integer' AND cleanup_days BETWEEN 0 AND 36500),
 revision INTEGER NOT NULL DEFAULT 1 CHECK(typeof(revision)='integer' AND revision BETWEEN 1 AND 9007199254740991),
 next_cleanup_at INTEGER CHECK(next_cleanup_at IS NULL OR (typeof(next_cleanup_at)='integer' AND next_cleanup_at BETWEEN 0 AND 9007199254740991)),
 CHECK(path IS NOT NULL OR next_cleanup_at IS NULL)
);
INSERT INTO recycle_bin_settings(media_type) VALUES('tv'),('movies');
CREATE TRIGGER recycle_bin_settings_retained BEFORE DELETE ON recycle_bin_settings
BEGIN SELECT RAISE(ABORT,'recycle bin settings are retained singletons'); END;
CREATE TRIGGER recycle_bin_settings_identity BEFORE UPDATE OF media_type ON recycle_bin_settings
WHEN NEW.media_type IS NOT OLD.media_type
BEGIN SELECT RAISE(ABORT,'recycle bin settings domain is immutable'); END;
-- Lexical, component-boundary overlap between a configured bin and every library declaration in
-- both domains. Library paths are compared without trailing separators; '/' overlaps everything.
-- This cannot detect bind-mount or symlink aliases; the deletion executor never follows symlinks.
CREATE TRIGGER recycle_bin_settings_overlap BEFORE UPDATE OF path ON recycle_bin_settings
WHEN NEW.path IS NOT NULL AND NEW.path IS NOT OLD.path AND EXISTS(SELECT 1 FROM (SELECT path p FROM root_folders UNION ALL SELECT path FROM series UNION ALL SELECT path FROM movies) WHERE rtrim(p,'/')=NEW.path OR substr(rtrim(p,'/'),1,length(NEW.path)+1)=NEW.path||'/' OR substr(NEW.path,1,length(rtrim(p,'/'))+1)=rtrim(p,'/')||'/')
BEGIN SELECT RAISE(ABORT,'recycle bin overlaps a library path'); END;
CREATE TRIGGER recycle_bin_root_folder_insert BEFORE INSERT ON root_folders
WHEN EXISTS(SELECT 1 FROM recycle_bin_settings b WHERE b.path IS NOT NULL AND (rtrim(NEW.path,'/')=b.path OR substr(rtrim(NEW.path,'/'),1,length(b.path)+1)=b.path||'/' OR substr(b.path,1,length(rtrim(NEW.path,'/'))+1)=rtrim(NEW.path,'/')||'/'))
BEGIN SELECT RAISE(ABORT,'library path overlaps the recycle bin'); END;
CREATE TRIGGER recycle_bin_root_folder_update BEFORE UPDATE OF path ON root_folders
WHEN NEW.path IS NOT OLD.path AND EXISTS(SELECT 1 FROM recycle_bin_settings b WHERE b.path IS NOT NULL AND (rtrim(NEW.path,'/')=b.path OR substr(rtrim(NEW.path,'/'),1,length(b.path)+1)=b.path||'/' OR substr(b.path,1,length(rtrim(NEW.path,'/'))+1)=rtrim(NEW.path,'/')||'/'))
BEGIN SELECT RAISE(ABORT,'library path overlaps the recycle bin'); END;
CREATE TRIGGER recycle_bin_series_insert BEFORE INSERT ON series
WHEN EXISTS(SELECT 1 FROM recycle_bin_settings b WHERE b.path IS NOT NULL AND (rtrim(NEW.path,'/')=b.path OR substr(rtrim(NEW.path,'/'),1,length(b.path)+1)=b.path||'/' OR substr(b.path,1,length(rtrim(NEW.path,'/'))+1)=rtrim(NEW.path,'/')||'/'))
BEGIN SELECT RAISE(ABORT,'library path overlaps the recycle bin'); END;
CREATE TRIGGER recycle_bin_series_update BEFORE UPDATE OF path ON series
WHEN NEW.path IS NOT OLD.path AND EXISTS(SELECT 1 FROM recycle_bin_settings b WHERE b.path IS NOT NULL AND (rtrim(NEW.path,'/')=b.path OR substr(rtrim(NEW.path,'/'),1,length(b.path)+1)=b.path||'/' OR substr(b.path,1,length(rtrim(NEW.path,'/'))+1)=rtrim(NEW.path,'/')||'/'))
BEGIN SELECT RAISE(ABORT,'library path overlaps the recycle bin'); END;
CREATE TRIGGER recycle_bin_movie_insert BEFORE INSERT ON movies
WHEN EXISTS(SELECT 1 FROM recycle_bin_settings b WHERE b.path IS NOT NULL AND (rtrim(NEW.path,'/')=b.path OR substr(rtrim(NEW.path,'/'),1,length(b.path)+1)=b.path||'/' OR substr(b.path,1,length(rtrim(NEW.path,'/'))+1)=rtrim(NEW.path,'/')||'/'))
BEGIN SELECT RAISE(ABORT,'library path overlaps the recycle bin'); END;
CREATE TRIGGER recycle_bin_movie_update BEFORE UPDATE OF path ON movies
WHEN NEW.path IS NOT OLD.path AND EXISTS(SELECT 1 FROM recycle_bin_settings b WHERE b.path IS NOT NULL AND (rtrim(NEW.path,'/')=b.path OR substr(rtrim(NEW.path,'/'),1,length(b.path)+1)=b.path||'/' OR substr(b.path,1,length(rtrim(NEW.path,'/'))+1)=rtrim(NEW.path,'/')||'/'))
BEGIN SELECT RAISE(ABORT,'library path overlaps the recycle bin'); END;

-- Journaled deletion intent. Rows are retained: history references them and recovery needs the
-- captured identity. Phases: admitted (intent recorded, disk effect may or may not have
-- happened), copied (cross-device copy published and verified; source untouched; aborts only if
-- that copy vanished before commit), committed
-- (association removed; cross-device source cleanup pending), complete, aborted (no change).
CREATE TABLE file_deletions (
 id TEXT PRIMARY KEY NOT NULL CHECK(typeof(id)='text' AND length(id)=36 AND substr(id,9,1)='-' AND substr(id,14,1)='-' AND substr(id,19,1)='-' AND substr(id,24,1)='-' AND length(replace(id,'-',''))=32 AND replace(id,'-','') NOT GLOB '*[^0-9a-f]*'),
 batch_id TEXT NOT NULL CHECK(typeof(batch_id)='text' AND length(batch_id)=36 AND substr(batch_id,9,1)='-' AND substr(batch_id,14,1)='-' AND substr(batch_id,19,1)='-' AND substr(batch_id,24,1)='-' AND length(replace(batch_id,'-',''))=32 AND replace(batch_id,'-','') NOT GLOB '*[^0-9a-f]*'),
 media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
 file_id INTEGER NOT NULL CHECK(typeof(file_id)='integer' AND file_id BETWEEN 1 AND 9007199254740991),
 owner_id INTEGER NOT NULL CHECK(typeof(owner_id)='integer' AND owner_id BETWEEN 1 AND 9007199254740991),
 path TEXT NOT NULL CHECK(typeof(path)='text' AND length(CAST(path AS BLOB)) BETWEEN 2 AND 4096 AND substr(path,1,1)='/' AND instr(path,char(0))=0),
 mode TEXT NOT NULL CHECK(mode IN ('recycle','permanent','absent')),
 bin_path TEXT CHECK(bin_path IS NULL OR (typeof(bin_path)='text' AND length(CAST(bin_path AS BLOB)) BETWEEN 2 AND 4096 AND substr(bin_path,1,1)='/' AND substr(bin_path,-1)!='/' AND instr(bin_path,char(0))=0)),
 destination TEXT CHECK(destination IS NULL OR (typeof(destination)='text' AND length(CAST(destination AS BLOB)) BETWEEN 3 AND 4096 AND instr(destination,char(0))=0)),
 source_dev TEXT CHECK(source_dev IS NULL OR (typeof(source_dev)='text' AND length(source_dev) BETWEEN 1 AND 20 AND source_dev NOT GLOB '*[^0-9]*')),
 source_ino TEXT CHECK(source_ino IS NULL OR (typeof(source_ino)='text' AND length(source_ino) BETWEEN 1 AND 20 AND source_ino NOT GLOB '*[^0-9]*')),
 source_size INTEGER CHECK(source_size IS NULL OR (typeof(source_size)='integer' AND source_size BETWEEN 0 AND 9007199254740991)),
 source_mtime INTEGER CHECK(source_mtime IS NULL OR typeof(source_mtime)='integer'),
 source_mtime_ns INTEGER CHECK(source_mtime_ns IS NULL OR (typeof(source_mtime_ns)='integer' AND source_mtime_ns BETWEEN 0 AND 999999999)),
 transfer TEXT CHECK(transfer IS NULL OR transfer IN ('rename','link','copy','unlink','none')),
 dest_dev TEXT CHECK(dest_dev IS NULL OR (typeof(dest_dev)='text' AND length(dest_dev) BETWEEN 1 AND 20 AND dest_dev NOT GLOB '*[^0-9]*')),
 dest_ino TEXT CHECK(dest_ino IS NULL OR (typeof(dest_ino)='text' AND length(dest_ino) BETWEEN 1 AND 20 AND dest_ino NOT GLOB '*[^0-9]*')),
 phase TEXT NOT NULL DEFAULT 'admitted' CHECK(phase IN ('admitted','copied','committed','complete','aborted')),
 error_code TEXT CHECK(error_code IS NULL OR (typeof(error_code)='text' AND length(error_code) BETWEEN 1 AND 64 AND error_code NOT GLOB '*[^a-z0-9_]*')),
 created_at INTEGER NOT NULL CHECK(typeof(created_at)='integer' AND created_at BETWEEN 0 AND 9007199254740991),
 updated_at INTEGER NOT NULL CHECK(typeof(updated_at)='integer' AND updated_at BETWEEN 0 AND 9007199254740991),
 CHECK((mode='recycle')=(bin_path IS NOT NULL AND destination IS NOT NULL)),
 CHECK(mode='recycle' OR (bin_path IS NULL AND destination IS NULL)),
 CHECK(destination IS NULL OR substr(destination,1,length(bin_path)+1)=bin_path||'/'),
 CHECK((source_dev IS NULL)=(source_ino IS NULL) AND (source_dev IS NULL)=(source_size IS NULL) AND (source_dev IS NULL)=(source_mtime IS NULL) AND (source_dev IS NULL)=(source_mtime_ns IS NULL)),
 CHECK((mode='absent')=(source_dev IS NULL)),
 CHECK((dest_dev IS NULL)=(dest_ino IS NULL)),
 CHECK(phase NOT IN ('copied','committed') OR (mode='recycle' AND transfer='copy' AND dest_dev IS NOT NULL)),
 CHECK(phase!='complete' OR transfer IS NOT NULL),
 CHECK(transfer IS NULL OR (mode='recycle' AND transfer IN ('rename','link','copy')) OR (mode='permanent' AND transfer='unlink') OR (mode='absent' AND transfer='none'))
);
CREATE UNIQUE INDEX file_deletions_active ON file_deletions(media_type,file_id) WHERE phase IN ('admitted','copied','committed');
CREATE INDEX file_deletions_unfinished ON file_deletions(created_at,id) WHERE phase IN ('admitted','copied','committed');
CREATE TRIGGER file_deletions_intent BEFORE UPDATE ON file_deletions
BEGIN
 SELECT CASE WHEN NEW.id IS NOT OLD.id OR NEW.batch_id IS NOT OLD.batch_id OR NEW.media_type IS NOT OLD.media_type OR NEW.file_id IS NOT OLD.file_id OR NEW.owner_id IS NOT OLD.owner_id OR NEW.path IS NOT OLD.path OR NEW.mode IS NOT OLD.mode OR NEW.bin_path IS NOT OLD.bin_path OR NEW.destination IS NOT OLD.destination OR NEW.source_dev IS NOT OLD.source_dev OR NEW.source_ino IS NOT OLD.source_ino OR NEW.source_size IS NOT OLD.source_size OR NEW.source_mtime IS NOT OLD.source_mtime OR NEW.source_mtime_ns IS NOT OLD.source_mtime_ns OR NEW.created_at IS NOT OLD.created_at THEN RAISE(ABORT,'file deletion intent is immutable') END;
 SELECT CASE WHEN NOT ((OLD.phase='admitted' AND NEW.phase IN ('admitted','copied','complete','aborted')) OR (OLD.phase='copied' AND NEW.phase IN ('copied','committed','aborted')) OR (OLD.phase='committed' AND NEW.phase IN ('committed','complete'))) THEN RAISE(ABORT,'invalid file deletion transition') END;
 SELECT CASE WHEN OLD.dest_dev IS NOT NULL AND (NEW.dest_dev IS NOT OLD.dest_dev OR NEW.dest_ino IS NOT OLD.dest_ino) THEN RAISE(ABORT,'recycled copy identity is immutable') END;
 SELECT CASE WHEN OLD.transfer IS NOT NULL AND NEW.transfer IS NOT OLD.transfer THEN RAISE(ABORT,'file deletion transfer is immutable') END;
END;
CREATE TRIGGER file_deletions_retained BEFORE DELETE ON file_deletions
BEGIN SELECT RAISE(ABORT,'file deletion journal must be retained'); END;
-- Once intent is admitted, no import or scan may newly claim the file as its replacement original;
-- the deletion commit could otherwise be blocked by a RESTRICT reference after bytes moved.
CREATE TRIGGER file_deletion_rss_old_file BEFORE INSERT ON rss_candidate_imports
WHEN EXISTS(SELECT 1 FROM file_deletions d WHERE d.phase IN ('admitted','copied','committed') AND ((d.media_type='tv' AND d.file_id=NEW.old_episode_file_id) OR (d.media_type='movies' AND d.file_id=NEW.old_movie_file_id)))
BEGIN SELECT RAISE(ABORT,'file deletion is in progress'); END;
CREATE TRIGGER file_deletion_scan_old_file BEFORE INSERT ON scan_import_replacements
WHEN EXISTS(SELECT 1 FROM file_deletions d WHERE d.phase IN ('admitted','copied','committed') AND ((d.media_type='tv' AND d.file_id=NEW.old_episode_file_id) OR (d.media_type='movies' AND d.file_id=NEW.old_movie_file_id)))
BEGIN SELECT RAISE(ABORT,'file deletion is in progress'); END;
CREATE TRIGGER file_deletion_scan_child_movie BEFORE INSERT ON downloaded_scan_children
WHEN NEW.expected_old_movie_file_id IS NOT NULL AND EXISTS(SELECT 1 FROM file_deletions d WHERE d.phase IN ('admitted','copied','committed') AND d.media_type='movies' AND d.file_id=NEW.expected_old_movie_file_id)
BEGIN SELECT RAISE(ABORT,'file deletion is in progress'); END;
CREATE TRIGGER file_deletion_scan_child_movie_update BEFORE UPDATE OF expected_old_movie_file_id ON downloaded_scan_children
WHEN NEW.expected_old_movie_file_id IS NOT NULL AND NEW.expected_old_movie_file_id IS NOT OLD.expected_old_movie_file_id AND EXISTS(SELECT 1 FROM file_deletions d WHERE d.phase IN ('admitted','copied','committed') AND d.media_type='movies' AND d.file_id=NEW.expected_old_movie_file_id)
BEGIN SELECT RAISE(ABORT,'file deletion is in progress'); END;
CREATE TRIGGER file_deletion_scan_child_episode BEFORE INSERT ON downloaded_scan_child_episodes
WHEN NEW.expected_old_episode_file_id IS NOT NULL AND EXISTS(SELECT 1 FROM file_deletions d WHERE d.phase IN ('admitted','copied','committed') AND d.media_type='tv' AND d.file_id=NEW.expected_old_episode_file_id)
BEGIN SELECT RAISE(ABORT,'file deletion is in progress'); END;

-- One immutable event per detached target (several for a shared multi-episode file).
CREATE TABLE file_deletion_history (
 id TEXT PRIMARY KEY NOT NULL CHECK(typeof(id)='text' AND length(id)=36 AND substr(id,9,1)='-' AND substr(id,14,1)='-' AND substr(id,19,1)='-' AND substr(id,24,1)='-' AND length(replace(id,'-',''))=32 AND replace(id,'-','') NOT GLOB '*[^0-9a-f]*'),
 deletion_id TEXT NOT NULL REFERENCES file_deletions(id) ON DELETE RESTRICT,
 media_type TEXT NOT NULL CHECK(media_type IN ('episode','movie')),
 episode_id INTEGER CHECK(episode_id IS NULL OR (typeof(episode_id)='integer' AND episode_id BETWEEN 1 AND 9007199254740991)),
 movie_id INTEGER CHECK(movie_id IS NULL OR (typeof(movie_id)='integer' AND movie_id BETWEEN 1 AND 9007199254740991)),
 file_id INTEGER NOT NULL CHECK(typeof(file_id)='integer' AND file_id BETWEEN 1 AND 9007199254740991),
 path TEXT NOT NULL CHECK(typeof(path)='text' AND length(CAST(path AS BLOB)) BETWEEN 2 AND 4096),
 recycle_path TEXT CHECK(recycle_path IS NULL OR (typeof(recycle_path)='text' AND length(CAST(recycle_path AS BLOB)) BETWEEN 3 AND 4096)),
 reason TEXT NOT NULL CHECK(reason='manual'),
 deleted_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
 CHECK((media_type='episode' AND episode_id IS NOT NULL AND movie_id IS NULL) OR (media_type='movie' AND movie_id IS NOT NULL AND episode_id IS NULL))
);
CREATE UNIQUE INDEX file_deletion_history_target ON file_deletion_history(deletion_id,media_type,coalesce(episode_id,movie_id));
CREATE INDEX file_deletion_history_order ON file_deletion_history(deleted_at DESC,id DESC);
CREATE INDEX file_deletion_history_episode ON file_deletion_history(episode_id,deleted_at);
CREATE INDEX file_deletion_history_movie ON file_deletion_history(movie_id,deleted_at);
CREATE TRIGGER file_deletion_history_owner BEFORE INSERT ON file_deletion_history
WHEN NOT EXISTS(SELECT 1 FROM file_deletions d WHERE d.id=NEW.deletion_id AND d.file_id=NEW.file_id AND d.path=NEW.path AND d.destination IS NEW.recycle_path AND d.phase IN ('admitted','copied') AND ((NEW.media_type='episode' AND d.media_type='tv') OR (NEW.media_type='movie' AND d.media_type='movies' AND d.owner_id=NEW.movie_id)))
BEGIN SELECT RAISE(ABORT,'file deletion history ownership mismatch'); END;
CREATE TRIGGER file_deletion_history_immutable BEFORE UPDATE ON file_deletion_history
BEGIN SELECT RAISE(ABORT,'file deletion history is immutable'); END;
CREATE TRIGGER file_deletion_history_retained BEFORE DELETE ON file_deletion_history
BEGIN SELECT RAISE(ABORT,'file deletion history must be retained'); END;

-- CleanUpRecycleBin: manual or scheduled, one active command per domain, shared capacity pool.
CREATE TABLE recycle_bin_cleanup_commands (
 id TEXT PRIMARY KEY NOT NULL CHECK(typeof(id)='text' AND length(id)=36 AND substr(id,9,1)='-' AND substr(id,14,1)='-' AND substr(id,19,1)='-' AND substr(id,24,1)='-' AND length(replace(id,'-',''))=32 AND replace(id,'-','') NOT GLOB '*[^0-9a-f]*'),
 name TEXT NOT NULL CHECK(name='clean_up_recycle_bin'),
 media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
 origin TEXT NOT NULL CHECK(origin IN ('manual','automatic')),
 priority INTEGER NOT NULL DEFAULT 0 CHECK(typeof(priority)='integer' AND priority IN (0,1)),
 status TEXT NOT NULL DEFAULT 'queued' CHECK(status IN ('queued','running','retry_wait','succeeded','failed','cancelled')),
 attempts INTEGER NOT NULL DEFAULT 0 CHECK(typeof(attempts)='integer' AND attempts BETWEEN 0 AND 3),
 next_attempt_at INTEGER NOT NULL CHECK(typeof(next_attempt_at)='integer' AND next_attempt_at BETWEEN 0 AND 9007199254740991),
 created_at INTEGER NOT NULL CHECK(typeof(created_at)='integer' AND created_at BETWEEN 0 AND 9007199254740991),
 started_at INTEGER CHECK(started_at IS NULL OR (typeof(started_at)='integer' AND started_at BETWEEN 0 AND 9007199254740991)),
 completed_at INTEGER CHECK(completed_at IS NULL OR (typeof(completed_at)='integer' AND completed_at BETWEEN 0 AND 9007199254740991)),
 error_code TEXT CHECK(error_code IS NULL OR error_code IN ('interrupted','storage_error','import_busy','recycle_bin_unavailable','filesystem_error')),
 outcome TEXT CHECK(outcome IS NULL OR outcome IN ('cleaned','not_configured','disabled')),
 bin_path TEXT CHECK(bin_path IS NULL OR (typeof(bin_path)='text' AND length(CAST(bin_path AS BLOB)) BETWEEN 2 AND 4096 AND instr(bin_path,char(0))=0)),
 cleanup_days INTEGER CHECK(cleanup_days IS NULL OR (typeof(cleanup_days)='integer' AND cleanup_days BETWEEN 0 AND 36500)),
 files_removed INTEGER NOT NULL DEFAULT 0 CHECK(typeof(files_removed)='integer' AND files_removed BETWEEN 0 AND 9007199254740991),
 folders_removed INTEGER NOT NULL DEFAULT 0 CHECK(typeof(folders_removed)='integer' AND folders_removed BETWEEN 0 AND 9007199254740991),
 entries_skipped INTEGER NOT NULL DEFAULT 0 CHECK(typeof(entries_skipped)='integer' AND entries_skipped BETWEEN 0 AND 9007199254740991),
 entries_failed INTEGER NOT NULL DEFAULT 0 CHECK(typeof(entries_failed)='integer' AND entries_failed BETWEEN 0 AND 9007199254740991),
 bounded INTEGER NOT NULL DEFAULT 0 CHECK(typeof(bounded)='integer' AND bounded IN (0,1)),
 CHECK((status IN ('queued','running','retry_wait') AND completed_at IS NULL) OR (status IN ('succeeded','failed','cancelled') AND completed_at IS NOT NULL)),
 CHECK(status!='queued' OR (attempts=0 AND started_at IS NULL)),
 CHECK(status NOT IN ('running','succeeded','retry_wait') OR (attempts>0 AND started_at IS NOT NULL)),
 CHECK(status!='retry_wait' OR attempts<3),
 CHECK((status IN ('failed','retry_wait') AND error_code IS NOT NULL) OR (status NOT IN ('failed','retry_wait') AND error_code IS NULL)),
 CHECK((status='succeeded')=(outcome IS NOT NULL)),
 CHECK(status='succeeded' OR (files_removed=0 AND folders_removed=0 AND entries_skipped=0 AND entries_failed=0 AND bounded=0)),
 CHECK(outcome IS NOT 'cleaned' OR (bin_path IS NOT NULL AND cleanup_days>0))
);
CREATE UNIQUE INDEX recycle_bin_cleanup_active ON recycle_bin_cleanup_commands(media_type) WHERE status IN ('queued','running','retry_wait');
CREATE INDEX recycle_bin_cleanup_ready ON recycle_bin_cleanup_commands(status,next_attempt_at,priority,created_at,id);
CREATE INDEX recycle_bin_cleanup_history ON recycle_bin_cleanup_commands(created_at,id);
CREATE TRIGGER recycle_bin_cleanup_admit BEFORE INSERT ON recycle_bin_cleanup_commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM recycle_bin_cleanup_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 OR NEW.outcome IS NOT NULL OR NEW.bin_path IS NOT NULL OR NEW.cleanup_days IS NOT NULL THEN RAISE(ABORT,'commands must be enqueued') END;
END;
CREATE TRIGGER recycle_bin_cleanup_transition BEFORE UPDATE ON recycle_bin_cleanup_commands
BEGIN
 SELECT CASE WHEN NEW.id IS NOT OLD.id OR NEW.name IS NOT OLD.name OR NEW.media_type IS NOT OLD.media_type OR NEW.origin IS NOT OLD.origin OR NEW.created_at IS NOT OLD.created_at OR NEW.priority IS NOT OLD.priority THEN RAISE(ABORT,'command identity is immutable') END;
 SELECT CASE WHEN NOT ((OLD.status IN ('queued','retry_wait') AND NEW.status IN ('running','cancelled','failed')) OR (OLD.status='running' AND NEW.status IN ('running','retry_wait','succeeded','failed','cancelled'))) THEN RAISE(ABORT,'invalid command transition') END;
 SELECT CASE WHEN NEW.attempts != OLD.attempts + CASE WHEN NEW.status='running' AND OLD.status IN ('queued','retry_wait') THEN 1 ELSE 0 END THEN RAISE(ABORT,'invalid command attempt counter') END;
END;
CREATE TRIGGER recycle_bin_cleanup_delete_terminal BEFORE DELETE ON recycle_bin_cleanup_commands WHEN OLD.status IN ('queued','running','retry_wait')
BEGIN SELECT RAISE(ABORT,'active commands cannot be deleted'); END;

-- hc.036 RecyclingBinCheck, one scoped identity per domain. Starts unobserved.
INSERT INTO health_checks(scope,check_key,startup,scheduled,compatibility_type) VALUES
 ('tv','recycle_bin',1,1,'RecyclingBinCheck'),
 ('movies','recycle_bin',1,1,'RecyclingBinCheck');
-- Every existing pool admission also counts active cleanup commands.
DROP TRIGGER commands_admit;
CREATE TRIGGER commands_admit BEFORE INSERT ON commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM recycle_bin_cleanup_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.provider_id AND p.revision=NEW.provider_revision AND p.enabled=1 AND p.implementation IN (SELECT implementation FROM provider_implementations WHERE kind='download_client') AND s.media_type=NEW.media_type) THEN RAISE(ABORT,'invalid refresh provider scope or revision') END;
END;
DROP TRIGGER metadata_refresh_admit;
CREATE TRIGGER metadata_refresh_admit BEFORE INSERT ON metadata_refresh_commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM recycle_bin_cleanup_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM series s WHERE s.id=NEW.series_id AND s.tvdb_id=NEW.external_id) AND NOT EXISTS(SELECT 1 FROM movies m JOIN movie_metadata d ON d.id=m.metadata_id WHERE m.id=NEW.movie_id AND m.metadata_id=NEW.metadata_id AND d.tmdb_id=NEW.external_id) THEN RAISE(ABORT,'invalid metadata refresh target') END;
END;
DROP TRIGGER blocklist_clear_admit;
CREATE TRIGGER blocklist_clear_admit BEFORE INSERT ON blocklist_clear_commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM recycle_bin_cleanup_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
END;
DROP TRIGGER rss_commands_admit;
CREATE TRIGGER rss_commands_admit BEFORE INSERT ON rss_commands BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM recycle_bin_cleanup_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 OR NEW.fetch_complete!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT (EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.indexer_id AND p.revision=NEW.indexer_revision AND p.implementation IN ('torznab','newznab') AND s.media_type=NEW.media_type AND p.enabled=1) AND (NEW.selection_kind='default' OR EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.client_id AND p.revision=NEW.client_revision AND p.implementation IN (SELECT c.implementation FROM provider_implementations c JOIN provider_implementations i ON i.protocol=c.protocol JOIN providers x ON x.implementation=i.implementation WHERE c.kind='download_client' AND x.id=NEW.indexer_id) AND s.media_type=NEW.media_type AND p.enabled=1))) THEN RAISE(ABORT,'invalid RSS provider scope or revision') END;
END;
DROP TRIGGER search_commands_admit;
CREATE TRIGGER search_commands_admit BEFORE INSERT ON search_commands BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM recycle_bin_cleanup_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 OR NEW.fetch_complete!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT (EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.indexer_id AND p.revision=NEW.indexer_revision AND p.implementation IN ('torznab','newznab') AND s.media_type=NEW.media_type AND p.enabled=1) AND (NEW.selection_kind='default' OR EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.client_id AND p.revision=NEW.client_revision AND p.implementation IN (SELECT c.implementation FROM provider_implementations c JOIN provider_implementations i ON i.protocol=c.protocol JOIN providers x ON x.implementation=i.implementation WHERE c.kind='download_client' AND x.id=NEW.indexer_id) AND s.media_type=NEW.media_type AND p.enabled=1))) THEN RAISE(ABORT,'invalid RSS provider scope or revision') END;
 SELECT CASE WHEN NOT ((NEW.media_type='tv' AND EXISTS(SELECT 1 FROM episodes e WHERE e.id=NEW.requested_episode_id AND json_extract(NEW.captured_target_json,'$.media_type')='tv' AND json_extract(NEW.captured_target_json,'$.episode_id')=e.id AND json_extract(NEW.captured_target_json,'$.series_id')=e.series_id)) OR (NEW.media_type='movies' AND EXISTS(SELECT 1 FROM movies m WHERE m.id=NEW.requested_movie_id AND json_extract(NEW.captured_target_json,'$.media_type')='movies' AND json_extract(NEW.captured_target_json,'$.movie_id')=m.id AND json_extract(NEW.captured_target_json,'$.metadata_id')=m.metadata_id))) THEN RAISE(ABORT,'invalid search target') END;
END;
DROP TRIGGER manual_import_commands_admit;
CREATE TRIGGER manual_import_commands_admit BEFORE INSERT ON manual_import_commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM recycle_bin_cleanup_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM import_journal WHERE operation_id=NEW.operation_id) THEN RAISE(ABORT,'operation has no import journal') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM rss_candidate_imports WHERE operation_id=NEW.operation_id) THEN RAISE(ABORT,'operation is owned by automated download import') END;
END;
DROP TRIGGER quality_reset_admit;
CREATE TRIGGER quality_reset_admit BEFORE INSERT ON quality_reset_commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM recycle_bin_cleanup_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
END;
DROP TRIGGER rescan_admit;
CREATE TRIGGER rescan_admit BEFORE INSERT ON rescan_commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM recycle_bin_cleanup_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT ((NEW.media_type='tv' AND EXISTS(SELECT 1 FROM series WHERE id=NEW.series_id)) OR (NEW.media_type='movies' AND EXISTS(SELECT 1 FROM movies WHERE id=NEW.movie_id))) THEN RAISE(ABORT,'invalid rescan target') END;
 -- 'preview' does not touch the filesystem yet and may never be executed, so it alone must not
 -- block a rescan forever; 'complete' is done. The window that matters is real transfer work.
 SELECT CASE WHEN EXISTS(SELECT 1 FROM import_journal j JOIN operations o ON o.id=j.operation_id WHERE j.phase NOT IN ('preview','complete') AND ((NEW.media_type='tv' AND o.media_type='episode' AND EXISTS(SELECT 1 FROM episodes e WHERE e.id=o.episode_id AND e.series_id=NEW.series_id)) OR (NEW.media_type='movies' AND o.media_type='movie' AND o.movie_id=NEW.movie_id))) THEN RAISE(ABORT,'rescan target has an in-flight import') END;
 -- 'queued' has not started preflight/transfer work; 'blocked'/'cancelled' are paused, not active.
 SELECT CASE WHEN EXISTS(SELECT 1 FROM download_processing dp JOIN rss_candidates r ON r.id=dp.candidate_id WHERE dp.status IN ('checking','importing') AND ((NEW.media_type='tv' AND r.media_type='tv' AND r.series_id=NEW.series_id) OR (NEW.media_type='movies' AND r.media_type='movies' AND r.movie_id=NEW.movie_id))) THEN RAISE(ABORT,'rescan target has in-flight download processing') END;
END;
DROP TRIGGER health_admit;
CREATE TRIGGER health_admit BEFORE INSERT ON health_commands BEGIN SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM recycle_bin_cleanup_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END; SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 OR NEW.epoch IS NOT (SELECT epoch FROM health_lifecycle WHERE id=1) THEN RAISE(ABORT,'invalid health admission') END; END;
DROP TRIGGER downloaded_scan_admit;
CREATE TRIGGER downloaded_scan_admit BEFORE INSERT ON downloaded_scan_commands BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM recycle_bin_cleanup_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 OR NEW.run_generation!=0 OR NEW.cancel_requested!=0 OR NEW.inventory_revision!=0 OR NEW.inventory_count!=0 OR NEW.inventory_bytes!=0 OR NEW.inventory_json IS NOT NULL OR NEW.candidate_id IS NOT NULL OR NEW.cleanup_json IS NOT NULL OR NEW.cleanup_resume_generation!=0 OR NEW.cleanup_resume_requested!=0 OR NEW.ready_order_at!=NEW.created_at THEN RAISE(ABORT,'scan must be queued without resolved authority') END;
 SELECT CASE WHEN NEW.supersedes_command_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM downloaded_scan_commands p WHERE p.id=NEW.supersedes_command_id AND p.media_type=NEW.media_type AND p.source_path=NEW.source_path AND p.status IN ('succeeded','failed','cancelled') AND p.coverage_state='incomplete' AND NOT EXISTS(SELECT 1 FROM downloaded_scan_reserved_children c WHERE c.command_id=p.id)) THEN RAISE(ABORT,'scan successor requires settled incomplete predecessor') END;
END;
