-- Upgrade retirement into the per-domain recycle bin, both domains.
-- An import that replaces an existing file first moves the original into its private recovery
-- artifact (retirement_state 'quarantined', unchanged since 0026/0054). When the domain has a
-- recycle bin configured at that point, this journal moves the artifact into the bin before the
-- import may complete. With no bin configured no row is written and the artifact is retained as
-- before. Rows are written before any filesystem effect and are never deleted.
-- Phases: admitted (intent and captured artifact identity; the move may or may not have
-- happened), copied (cross-device copy published and verified; artifact untouched), complete
-- (the bytes are in the bin and history is written), retained (recycling was refused or failed;
-- the artifact keeps the bytes and error_code says why). complete and retained are terminal.
CREATE TABLE upgrade_recycles (
 id TEXT PRIMARY KEY NOT NULL CHECK(typeof(id)='text' AND length(id)=36 AND substr(id,9,1)='-' AND substr(id,14,1)='-' AND substr(id,19,1)='-' AND substr(id,24,1)='-' AND length(replace(id,'-',''))=32 AND replace(id,'-','') NOT GLOB '*[^0-9a-f]*'),
 operation_id TEXT NOT NULL REFERENCES operations(id) ON DELETE RESTRICT,
 media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
 old_file_id INTEGER NOT NULL CHECK(typeof(old_file_id)='integer' AND old_file_id BETWEEN 1 AND 9007199254740991),
 owner_id INTEGER NOT NULL CHECK(typeof(owner_id)='integer' AND owner_id BETWEEN 1 AND 9007199254740991),
 original_path TEXT NOT NULL CHECK(typeof(original_path)='text' AND length(CAST(original_path AS BLOB)) BETWEEN 2 AND 4096 AND substr(original_path,1,1)='/' AND instr(original_path,char(0))=0),
 source TEXT NOT NULL CHECK(typeof(source)='text' AND length(CAST(source AS BLOB)) BETWEEN 2 AND 4096 AND substr(source,1,1)='/' AND instr(source,char(0))=0),
 bin_path TEXT NOT NULL CHECK(typeof(bin_path)='text' AND length(CAST(bin_path AS BLOB)) BETWEEN 2 AND 4096 AND substr(bin_path,1,1)='/' AND substr(bin_path,-1)!='/' AND instr(bin_path,char(0))=0),
 destination TEXT CHECK(destination IS NULL OR (typeof(destination)='text' AND length(CAST(destination AS BLOB)) BETWEEN 3 AND 4096 AND instr(destination,char(0))=0 AND substr(destination,1,length(bin_path)+1)=bin_path||'/')),
 source_dev TEXT CHECK(source_dev IS NULL OR (typeof(source_dev)='text' AND length(source_dev) BETWEEN 1 AND 20 AND source_dev NOT GLOB '*[^0-9]*')),
 source_ino TEXT CHECK(source_ino IS NULL OR (typeof(source_ino)='text' AND length(source_ino) BETWEEN 1 AND 20 AND source_ino NOT GLOB '*[^0-9]*')),
 source_size INTEGER CHECK(source_size IS NULL OR (typeof(source_size)='integer' AND source_size BETWEEN 0 AND 9007199254740991)),
 source_mtime INTEGER CHECK(source_mtime IS NULL OR typeof(source_mtime)='integer'),
 source_mtime_ns INTEGER CHECK(source_mtime_ns IS NULL OR (typeof(source_mtime_ns)='integer' AND source_mtime_ns BETWEEN 0 AND 999999999)),
 transfer TEXT CHECK(transfer IS NULL OR transfer IN ('rename','link','copy')),
 dest_dev TEXT CHECK(dest_dev IS NULL OR (typeof(dest_dev)='text' AND length(dest_dev) BETWEEN 1 AND 20 AND dest_dev NOT GLOB '*[^0-9]*')),
 dest_ino TEXT CHECK(dest_ino IS NULL OR (typeof(dest_ino)='text' AND length(dest_ino) BETWEEN 1 AND 20 AND dest_ino NOT GLOB '*[^0-9]*')),
 phase TEXT NOT NULL DEFAULT 'admitted' CHECK(phase IN ('admitted','copied','complete','retained')),
 error_code TEXT CHECK(error_code IS NULL OR (typeof(error_code)='text' AND length(error_code) BETWEEN 1 AND 64 AND error_code NOT GLOB '*[^a-z0-9_]*')),
 created_at INTEGER NOT NULL CHECK(typeof(created_at)='integer' AND created_at BETWEEN 0 AND 9007199254740991),
 updated_at INTEGER NOT NULL CHECK(typeof(updated_at)='integer' AND updated_at BETWEEN 0 AND 9007199254740991),
 CHECK((source_dev IS NULL)=(source_ino IS NULL) AND (source_dev IS NULL)=(source_size IS NULL) AND (source_dev IS NULL)=(source_mtime IS NULL) AND (source_dev IS NULL)=(source_mtime_ns IS NULL)),
 CHECK((dest_dev IS NULL)=(dest_ino IS NULL)),
 CHECK(phase='retained' OR (destination IS NOT NULL AND source_dev IS NOT NULL)),
 CHECK(phase!='copied' OR (transfer='copy' AND dest_dev IS NOT NULL)),
 CHECK(phase!='complete' OR transfer IS NOT NULL),
 CHECK(phase!='retained' OR error_code IS NOT NULL)
);
-- One TV file that served several episodes is one original, so it is recycled once.
CREATE UNIQUE INDEX upgrade_recycles_original ON upgrade_recycles(operation_id,media_type,old_file_id);
CREATE INDEX upgrade_recycles_unsettled ON upgrade_recycles(created_at,id) WHERE phase IN ('admitted','copied');
-- Admission requires the committed import, its quarantined original (same typed file id and
-- recorded path) and the domain's currently configured bin.
CREATE TRIGGER upgrade_recycles_admit BEFORE INSERT ON upgrade_recycles
BEGIN
 SELECT CASE WHEN NEW.phase NOT IN ('admitted','retained') OR NEW.transfer IS NOT NULL OR NEW.dest_dev IS NOT NULL THEN RAISE(ABORT,'invalid upgrade recycle admission') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM import_journal j JOIN operations o ON o.id=j.operation_id WHERE j.operation_id=NEW.operation_id AND j.phase='committed' AND o.media_type=CASE NEW.media_type WHEN 'tv' THEN 'episode' ELSE 'movie' END) THEN RAISE(ABORT,'upgrade recycle requires a committed import') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM rss_candidate_imports i WHERE i.operation_id=NEW.operation_id AND i.retirement_state='quarantined' AND json_extract(i.old_file_json,'$.path')=NEW.original_path AND ((NEW.media_type='tv' AND i.old_episode_file_id=NEW.old_file_id) OR (NEW.media_type='movies' AND i.old_movie_file_id=NEW.old_file_id)))
  AND NOT EXISTS(SELECT 1 FROM scan_import_replacements r WHERE r.operation_id=NEW.operation_id AND r.media_type=NEW.media_type AND r.old_file_id=NEW.old_file_id AND r.retirement_state='quarantined' AND json_extract(r.old_file_json,'$.path')=NEW.original_path)
  THEN RAISE(ABORT,'upgrade recycle requires a quarantined original') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM recycle_bin_settings b WHERE b.media_type=NEW.media_type AND b.path=NEW.bin_path) THEN RAISE(ABORT,'upgrade recycle requires the configured recycle bin') END;
END;
-- An import cannot complete while one of its originals is between the artifact and the bin.
CREATE TRIGGER upgrade_recycles_settle_before_complete BEFORE UPDATE OF phase ON import_journal
WHEN NEW.phase='complete' AND EXISTS(SELECT 1 FROM upgrade_recycles r WHERE r.operation_id=NEW.operation_id AND r.phase IN ('admitted','copied'))
BEGIN SELECT RAISE(ABORT,'upgrade recycle must settle before import completion'); END;

-- One immutable event per affected target (several for one multi-episode original).
CREATE TABLE upgrade_recycle_history (
 id TEXT PRIMARY KEY NOT NULL CHECK(typeof(id)='text' AND length(id)=36 AND substr(id,9,1)='-' AND substr(id,14,1)='-' AND substr(id,19,1)='-' AND substr(id,24,1)='-' AND length(replace(id,'-',''))=32 AND replace(id,'-','') NOT GLOB '*[^0-9a-f]*'),
 recycle_id TEXT NOT NULL REFERENCES upgrade_recycles(id) ON DELETE RESTRICT,
 media_type TEXT NOT NULL CHECK(media_type IN ('episode','movie')),
 episode_id INTEGER CHECK(episode_id IS NULL OR (typeof(episode_id)='integer' AND episode_id BETWEEN 1 AND 9007199254740991)),
 movie_id INTEGER CHECK(movie_id IS NULL OR (typeof(movie_id)='integer' AND movie_id BETWEEN 1 AND 9007199254740991)),
 file_id INTEGER NOT NULL CHECK(typeof(file_id)='integer' AND file_id BETWEEN 1 AND 9007199254740991),
 path TEXT NOT NULL CHECK(typeof(path)='text' AND length(CAST(path AS BLOB)) BETWEEN 2 AND 4096),
 recycle_path TEXT NOT NULL CHECK(typeof(recycle_path)='text' AND length(CAST(recycle_path AS BLOB)) BETWEEN 3 AND 4096),
 reason TEXT NOT NULL CHECK(reason='upgrade'),
 deleted_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
 CHECK((media_type='episode' AND episode_id IS NOT NULL AND movie_id IS NULL) OR (media_type='movie' AND movie_id IS NOT NULL AND episode_id IS NULL))
);
CREATE UNIQUE INDEX upgrade_recycle_history_target ON upgrade_recycle_history(recycle_id,media_type,coalesce(episode_id,movie_id));
CREATE INDEX upgrade_recycle_history_order ON upgrade_recycle_history(deleted_at DESC,id DESC);
CREATE INDEX upgrade_recycle_history_episode ON upgrade_recycle_history(episode_id,deleted_at);
CREATE INDEX upgrade_recycle_history_movie ON upgrade_recycle_history(movie_id,deleted_at);
CREATE TRIGGER upgrade_recycle_history_owner BEFORE INSERT ON upgrade_recycle_history
WHEN NOT EXISTS(SELECT 1 FROM upgrade_recycles r WHERE r.id=NEW.recycle_id AND r.old_file_id=NEW.file_id AND r.original_path=NEW.path AND r.destination=NEW.recycle_path AND r.phase IN ('admitted','copied') AND ((NEW.media_type='episode' AND r.media_type='tv') OR (NEW.media_type='movie' AND r.media_type='movies' AND r.owner_id=NEW.movie_id)))
BEGIN SELECT RAISE(ABORT,'upgrade recycle history ownership mismatch'); END;
CREATE TRIGGER upgrade_recycle_history_immutable BEFORE UPDATE ON upgrade_recycle_history
BEGIN SELECT RAISE(ABORT,'upgrade recycle history is immutable'); END;
CREATE TRIGGER upgrade_recycle_history_retained BEFORE DELETE ON upgrade_recycle_history
BEGIN SELECT RAISE(ABORT,'upgrade recycle history must be retained'); END;
CREATE TRIGGER upgrade_recycles_transition BEFORE UPDATE ON upgrade_recycles
BEGIN
 SELECT CASE WHEN NEW.id IS NOT OLD.id OR NEW.operation_id IS NOT OLD.operation_id OR NEW.media_type IS NOT OLD.media_type OR NEW.old_file_id IS NOT OLD.old_file_id OR NEW.owner_id IS NOT OLD.owner_id OR NEW.original_path IS NOT OLD.original_path OR NEW.source IS NOT OLD.source OR NEW.bin_path IS NOT OLD.bin_path OR NEW.destination IS NOT OLD.destination OR NEW.source_dev IS NOT OLD.source_dev OR NEW.source_ino IS NOT OLD.source_ino OR NEW.source_size IS NOT OLD.source_size OR NEW.source_mtime IS NOT OLD.source_mtime OR NEW.source_mtime_ns IS NOT OLD.source_mtime_ns OR NEW.created_at IS NOT OLD.created_at THEN RAISE(ABORT,'upgrade recycle intent is immutable') END;
 SELECT CASE WHEN NOT ((OLD.phase='admitted' AND NEW.phase IN ('admitted','copied','complete','retained')) OR (OLD.phase='copied' AND NEW.phase IN ('copied','complete','retained'))) THEN RAISE(ABORT,'invalid upgrade recycle transition') END;
 SELECT CASE WHEN OLD.dest_dev IS NOT NULL AND (NEW.dest_dev IS NOT OLD.dest_dev OR NEW.dest_ino IS NOT OLD.dest_ino) THEN RAISE(ABORT,'recycled copy identity is immutable') END;
 SELECT CASE WHEN OLD.transfer IS NOT NULL AND NEW.transfer IS NOT OLD.transfer THEN RAISE(ABORT,'upgrade recycle transfer is immutable') END;
 SELECT CASE WHEN NEW.phase='complete' AND NOT EXISTS(SELECT 1 FROM upgrade_recycle_history h WHERE h.recycle_id=NEW.id) THEN RAISE(ABORT,'upgrade recycle completion requires history') END;
END;
CREATE TRIGGER upgrade_recycles_retained BEFORE DELETE ON upgrade_recycles
BEGIN SELECT RAISE(ABORT,'upgrade recycle journal must be retained'); END;
