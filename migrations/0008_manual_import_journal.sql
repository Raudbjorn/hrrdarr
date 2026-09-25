-- Local single-worker initial imports only; identity/content validation belongs to the service.
-- Legacy previews remain stored; execution requires a new validated preview/journal.
CREATE TABLE import_journal (
    operation_id TEXT PRIMARY KEY NOT NULL REFERENCES operations(id) ON DELETE RESTRICT,
    version INTEGER NOT NULL DEFAULT 1 CHECK (version=1),
    plan_json TEXT NOT NULL CHECK (length(CAST(plan_json AS BLOB))<=16384 AND json_valid(plan_json) AND json_type(plan_json)='object'),
    stage_json TEXT CHECK (stage_json IS NULL OR (length(CAST(stage_json AS BLOB))<=4096 AND json_valid(stage_json) AND json_type(stage_json)='object')),
    phase TEXT NOT NULL CHECK (phase IN ('preview','staging','staged','published','committed','complete')),
    error_code TEXT CHECK (error_code IS NULL OR length(error_code)<=64),
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK (phase IN ('preview','staging') OR stage_json IS NOT NULL)
);
CREATE INDEX import_journal_phase ON import_journal(phase,operation_id);
CREATE TRIGGER import_plan_immutable BEFORE UPDATE OF operation_id,version,plan_json ON import_journal
WHEN NEW.operation_id IS NOT OLD.operation_id OR NEW.version IS NOT OLD.version OR NEW.plan_json IS NOT OLD.plan_json
BEGIN SELECT RAISE(ABORT,'import plan is immutable'); END;
CREATE TRIGGER import_operation_immutable BEFORE UPDATE OF id,media_type,episode_id,movie_id,source,mode,destination ON operations
WHEN EXISTS(SELECT 1 FROM import_journal WHERE operation_id=OLD.id)
 AND (NEW.id IS NOT OLD.id OR NEW.media_type IS NOT OLD.media_type OR NEW.episode_id IS NOT OLD.episode_id
 OR NEW.movie_id IS NOT OLD.movie_id OR NEW.source IS NOT OLD.source OR NEW.mode IS NOT OLD.mode OR NEW.destination IS NOT OLD.destination)
BEGIN SELECT RAISE(ABORT,'journaled import identity is immutable'); END;

-- File IDs are historical facts, deliberately not live FKs: later explicit retirement must
-- retain the import event. Validate ownership at insertion, never infer live files from history.
CREATE TABLE import_history (
    operation_id TEXT PRIMARY KEY NOT NULL REFERENCES import_journal(operation_id) ON DELETE RESTRICT,
    media_type TEXT NOT NULL CHECK (media_type IN ('episode','movie')),
    episode_id INTEGER REFERENCES episodes(id) ON DELETE RESTRICT,
    movie_id INTEGER REFERENCES movies(id) ON DELETE RESTRICT,
    episode_file_id INTEGER,
    movie_file_id INTEGER,
    source TEXT NOT NULL CHECK (length(CAST(source AS BLOB)) BETWEEN 1 AND 4096),
    destination TEXT NOT NULL CHECK (length(CAST(destination AS BLOB)) BETWEEN 1 AND 4096),
    size INTEGER NOT NULL CHECK (size>=0),
    sha256 TEXT NOT NULL CHECK (length(sha256)=64 AND sha256 NOT GLOB '*[^0-9a-f]*'),
    imported_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK ((media_type='episode' AND episode_id IS NOT NULL AND movie_id IS NULL AND episode_file_id IS NOT NULL AND movie_file_id IS NULL)
        OR (media_type='movie' AND movie_id IS NOT NULL AND episode_id IS NULL AND movie_file_id IS NOT NULL AND episode_file_id IS NULL))
);
CREATE INDEX import_history_episode ON import_history(episode_id,imported_at);
CREATE INDEX import_history_movie ON import_history(movie_id,imported_at);
CREATE TRIGGER import_history_owner BEFORE INSERT ON import_history
WHEN NOT EXISTS(SELECT 1 FROM operations o WHERE o.id=NEW.operation_id AND o.media_type=NEW.media_type
 AND o.episode_id IS NEW.episode_id AND o.movie_id IS NEW.movie_id AND o.source=NEW.source AND o.destination=NEW.destination)
 OR (NEW.media_type='episode' AND NOT EXISTS(SELECT 1 FROM episodes e JOIN episode_files f ON f.id=e.episode_file_id AND f.series_id=e.series_id
     WHERE e.id=NEW.episode_id AND f.id=NEW.episode_file_id AND f.path=NEW.destination))
 OR (NEW.media_type='movie' AND NOT EXISTS(SELECT 1 FROM movie_files f WHERE f.movie_id=NEW.movie_id AND f.id=NEW.movie_file_id AND f.path=NEW.destination))
BEGIN SELECT RAISE(ABORT,'import history ownership mismatch'); END;
CREATE TRIGGER import_history_immutable BEFORE UPDATE ON import_history
BEGIN SELECT RAISE(ABORT,'import history is immutable'); END;
CREATE TRIGGER import_commit_requires_history BEFORE UPDATE OF phase ON import_journal
WHEN NEW.phase IN ('committed','complete') AND NOT EXISTS(SELECT 1 FROM import_history WHERE operation_id=NEW.operation_id)
BEGIN SELECT RAISE(ABORT,'committed import requires history'); END;
CREATE TRIGGER import_insert_requires_preview BEFORE INSERT ON import_journal
WHEN NEW.phase NOT IN ('preview','staging')
BEGIN SELECT RAISE(ABORT,'new import must begin before transfer'); END;

-- Recovery may retry a phase or advance past an already-observed effect, never regress.
CREATE TRIGGER import_phase_monotonic BEFORE UPDATE OF phase ON import_journal
WHEN (CASE NEW.phase WHEN 'preview' THEN 0 WHEN 'staging' THEN 1 WHEN 'staged' THEN 2 WHEN 'published' THEN 3 WHEN 'committed' THEN 4 WHEN 'complete' THEN 5 END)
   < (CASE OLD.phase WHEN 'preview' THEN 0 WHEN 'staging' THEN 1 WHEN 'staged' THEN 2 WHEN 'published' THEN 3 WHEN 'committed' THEN 4 WHEN 'complete' THEN 5 END)
BEGIN SELECT RAISE(ABORT,'import phase cannot regress'); END;
-- Stage creation/copy enriches identity in staging. Once validated, publication and cleanup
-- must retain its inode/content facts. Compare scalar JSON values, not JSON formatting.
CREATE TRIGGER import_stage_identity_immutable BEFORE UPDATE OF stage_json ON import_journal
WHEN OLD.phase IN ('staged','published','committed','complete') AND (
 json_extract(NEW.stage_json,'$.directory.dev','$.directory.ino','$.directory.size','$.directory.mtime','$.directory.mtime_ns','$.directory.ctime','$.directory.ctime_ns')
 IS NOT json_extract(OLD.stage_json,'$.directory.dev','$.directory.ino','$.directory.size','$.directory.mtime','$.directory.mtime_ns','$.directory.ctime','$.directory.ctime_ns')
 OR json_extract(NEW.stage_json,'$.file.dev','$.file.ino','$.file.size','$.file.mtime','$.file.mtime_ns','$.file.ctime','$.file.ctime_ns','$.sha256')
 IS NOT json_extract(OLD.stage_json,'$.file.dev','$.file.ino','$.file.size','$.file.mtime','$.file.mtime_ns','$.file.ctime','$.file.ctime_ns','$.sha256'))
BEGIN SELECT RAISE(ABORT,'validated stage identity is immutable'); END;
-- Initial Stage creation (possibly in the same write that advances to staged) must
-- initialize its nullable cleanup fields. Subsequent cleanup facts belong to committed.
CREATE TRIGGER import_cleanup_checkpoint BEFORE UPDATE OF stage_json ON import_journal
WHEN (
 json_extract(NEW.stage_json,'$.quarantine.dev','$.quarantine.ino','$.quarantine.size','$.quarantine.mtime','$.quarantine.mtime_ns','$.quarantine.ctime','$.quarantine.ctime_ns','$.source_retired')
 IS NOT json_extract(OLD.stage_json,'$.quarantine.dev','$.quarantine.ino','$.quarantine.size','$.quarantine.mtime','$.quarantine.mtime_ns','$.quarantine.ctime','$.quarantine.ctime_ns','$.source_retired'))
 AND OLD.phase NOT IN ('preview','staging')
 AND (OLD.phase!='committed' OR NEW.phase!='committed'
 OR (json_extract(OLD.stage_json,'$.source_retired')=1 AND json_extract(NEW.stage_json,'$.source_retired') IS NOT 1)
 OR (json_type(OLD.stage_json,'$.quarantine')='object' AND
 json_extract(NEW.stage_json,'$.quarantine.dev','$.quarantine.ino','$.quarantine.size','$.quarantine.mtime','$.quarantine.mtime_ns','$.quarantine.ctime','$.quarantine.ctime_ns')
 IS NOT json_extract(OLD.stage_json,'$.quarantine.dev','$.quarantine.ino','$.quarantine.size','$.quarantine.mtime','$.quarantine.mtime_ns','$.quarantine.ctime','$.quarantine.ctime_ns')))
BEGIN SELECT RAISE(ABORT,'invalid import cleanup checkpoint'); END;
CREATE TRIGGER import_history_no_delete BEFORE DELETE ON import_history
BEGIN SELECT RAISE(ABORT,'import history must be retained'); END;
