-- Backup (job.002): the backup policy singleton and the durable Backup command. Archives live on
-- disk in the configured folder and are listed from there; the database records only the policy
-- and command history. No key material is ever stored here: provider/notification credentials
-- stay AEAD-sealed and the key is supplied by the environment.
CREATE TABLE backup_settings (
 id INTEGER PRIMARY KEY NOT NULL CHECK(id=1),
 -- NULL means the default folder `Backups` beside the local database file.
 folder TEXT CHECK(folder IS NULL OR (typeof(folder)='text' AND length(CAST(folder AS BLOB)) BETWEEN 2 AND 4096 AND substr(folder,1,1)='/' AND substr(folder,-1)!='/' AND instr(folder,'//')=0 AND instr(folder,char(0))=0 AND folder NOT GLOB ('*['||char(1)||'-'||char(31)||char(127)||']*') AND instr(folder,char(92))=0 AND instr(folder||'/','/../')=0 AND instr(folder||'/','/./')=0)),
 interval_days INTEGER NOT NULL DEFAULT 7 CHECK(typeof(interval_days)='integer' AND interval_days BETWEEN 1 AND 7),
 retention_days INTEGER NOT NULL DEFAULT 28 CHECK(typeof(retention_days)='integer' AND retention_days BETWEEN 1 AND 90),
 next_backup_at INTEGER CHECK(next_backup_at IS NULL OR (typeof(next_backup_at)='integer' AND next_backup_at BETWEEN 0 AND 9007199254740991)),
 revision INTEGER NOT NULL DEFAULT 1 CHECK(typeof(revision)='integer' AND revision BETWEEN 1 AND 9007199254740991)
);
INSERT INTO backup_settings(id) VALUES(1);
CREATE TRIGGER backup_settings_retained BEFORE DELETE ON backup_settings
BEGIN SELECT RAISE(ABORT,'backup settings are a retained singleton'); END;

-- Backup command: manual or scheduled. At most one active command (unique index), so it is
-- independent of the shared 1024-row command pool. An interrupted run is never retried
-- automatically: recovery fails it as 'interrupted' (a restored snapshot also contains the row of
-- the command that produced it, still 'running').
CREATE TABLE backup_commands (
 id TEXT PRIMARY KEY NOT NULL CHECK(typeof(id)='text' AND length(id)=36 AND substr(id,9,1)='-' AND substr(id,14,1)='-' AND substr(id,19,1)='-' AND substr(id,24,1)='-' AND length(replace(id,'-',''))=32 AND replace(id,'-','') NOT GLOB '*[^0-9a-f]*'),
 name TEXT NOT NULL CHECK(name='backup'),
 backup_type TEXT NOT NULL CHECK(backup_type IN ('manual','scheduled')),
 status TEXT NOT NULL DEFAULT 'queued' CHECK(status IN ('queued','running','succeeded','failed','cancelled')),
 attempts INTEGER NOT NULL DEFAULT 0 CHECK(typeof(attempts)='integer' AND attempts BETWEEN 0 AND 1),
 next_attempt_at INTEGER NOT NULL CHECK(typeof(next_attempt_at)='integer' AND next_attempt_at BETWEEN 0 AND 9007199254740991),
 created_at INTEGER NOT NULL CHECK(typeof(created_at)='integer' AND created_at BETWEEN 0 AND 9007199254740991),
 started_at INTEGER CHECK(started_at IS NULL OR (typeof(started_at)='integer' AND started_at BETWEEN 0 AND 9007199254740991)),
 completed_at INTEGER CHECK(completed_at IS NULL OR (typeof(completed_at)='integer' AND completed_at BETWEEN 0 AND 9007199254740991)),
 error_code TEXT CHECK(error_code IS NULL OR error_code IN ('interrupted','storage_error','backup_local_only','backup_folder_unavailable','backup_folder_unsafe','backup_too_large','backup_timeout','snapshot_failed','snapshot_invalid','archive_failed','publish_failed')),
 backup_id TEXT CHECK(backup_id IS NULL OR (typeof(backup_id)='text' AND length(backup_id)=32 AND backup_id NOT GLOB '*[^0-9a-f]*')),
 backup_name TEXT CHECK(backup_name IS NULL OR (typeof(backup_name)='text' AND length(backup_name) BETWEEN 1 AND 128 AND backup_name NOT GLOB '*[^A-Za-z0-9._-]*')),
 size_bytes INTEGER CHECK(size_bytes IS NULL OR (typeof(size_bytes)='integer' AND size_bytes BETWEEN 0 AND 9007199254740991)),
 pruned INTEGER NOT NULL DEFAULT 0 CHECK(typeof(pruned)='integer' AND pruned BETWEEN 0 AND 100000),
 CHECK((status IN ('queued','running') AND completed_at IS NULL) OR (status IN ('succeeded','failed','cancelled') AND completed_at IS NOT NULL)),
 CHECK(status!='queued' OR (attempts=0 AND started_at IS NULL)),
 CHECK(status NOT IN ('running','succeeded') OR (attempts=1 AND started_at IS NOT NULL)),
 CHECK((status='failed')=(error_code IS NOT NULL)),
 CHECK((status='succeeded')=(backup_id IS NOT NULL AND backup_name IS NOT NULL AND size_bytes IS NOT NULL)),
 CHECK(status='succeeded' OR pruned=0)
);
CREATE UNIQUE INDEX backup_commands_active ON backup_commands((1)) WHERE status IN ('queued','running');
CREATE INDEX backup_commands_history ON backup_commands(created_at,id);
CREATE TRIGGER backup_commands_admit BEFORE INSERT ON backup_commands
BEGIN
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 OR NEW.backup_id IS NOT NULL OR NEW.backup_name IS NOT NULL OR NEW.size_bytes IS NOT NULL OR NEW.pruned!=0 THEN RAISE(ABORT,'backup commands must be enqueued') END;
END;
CREATE TRIGGER backup_commands_transition BEFORE UPDATE ON backup_commands
BEGIN
 SELECT CASE WHEN NEW.id IS NOT OLD.id OR NEW.name IS NOT OLD.name OR NEW.backup_type IS NOT OLD.backup_type OR NEW.created_at IS NOT OLD.created_at THEN RAISE(ABORT,'command identity is immutable') END;
 SELECT CASE WHEN NOT ((OLD.status='queued' AND NEW.status IN ('running','cancelled','failed')) OR (OLD.status='running' AND NEW.status IN ('succeeded','failed'))) THEN RAISE(ABORT,'invalid command transition') END;
END;
CREATE TRIGGER backup_commands_delete_active BEFORE DELETE ON backup_commands WHEN OLD.status IN ('queued','running')
BEGIN SELECT RAISE(ABORT,'active commands cannot be deleted'); END;
