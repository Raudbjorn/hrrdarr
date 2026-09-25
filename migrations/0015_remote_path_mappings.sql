-- Windows keys use Unicode lowercase in the validated writer; SQLite lower() is ASCII-only.
CREATE TABLE remote_path_mappings (
 id INTEGER PRIMARY KEY AUTOINCREMENT CHECK(id BETWEEN 1 AND 9007199254740991),
 media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
 host TEXT NOT NULL CHECK(typeof(host)='text' AND length(CAST(host AS BLOB)) BETWEEN 1 AND 253 AND host=lower(host) AND instr(host,char(0))=0),
 remote_path TEXT NOT NULL CHECK(typeof(remote_path)='text' AND length(CAST(remote_path AS BLOB)) BETWEEN 1 AND 4096 AND instr(remote_path,char(0))=0),
 remote_kind TEXT NOT NULL CHECK(remote_kind IN ('posix','windows')),
 remote_key TEXT NOT NULL CHECK(typeof(remote_key)='text' AND length(CAST(remote_key AS BLOB)) BETWEEN 1 AND 12288 AND instr(remote_key,char(0))=0 AND (remote_kind='windows' OR remote_key=remote_path)),
 local_path TEXT NOT NULL CHECK(typeof(local_path)='text' AND length(CAST(local_path AS BLOB)) BETWEEN 2 AND 4096 AND substr(local_path,1,1)='/' AND substr(local_path,-1)!='/' AND instr(local_path,'//')=0 AND instr(local_path,char(0))=0 AND instr(local_path,char(92))=0 AND instr(local_path||'/','/../')=0 AND instr(local_path||'/','/./')=0),
 revision INTEGER NOT NULL DEFAULT 1 CHECK(typeof(revision)='integer' AND revision BETWEEN 1 AND 9007199254740991),
 UNIQUE(media_type,host,remote_key)
);
CREATE TRIGGER remote_mapping_revision BEFORE UPDATE ON remote_path_mappings
WHEN NEW.id IS NOT OLD.id OR NEW.media_type IS NOT OLD.media_type OR NEW.revision IS NOT OLD.revision+1
BEGIN SELECT RAISE(ABORT,'mapping update requires stable identity and next revision'); END;
