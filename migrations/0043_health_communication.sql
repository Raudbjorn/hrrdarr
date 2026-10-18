-- Preserve retained diagnostic identities, including events whose command was pruned.
-- This one-row migration scratch state validates and carries the AUTOINCREMENT high-water.
CREATE TABLE health_transition_migration_sequence (
 high_water INTEGER NOT NULL CHECK(typeof(high_water)='integer' AND high_water BETWEEN 0 AND 9007199254740991),
 entries INTEGER NOT NULL CHECK(entries BETWEEN 0 AND 1),
 valid INTEGER NOT NULL CHECK(valid=1)
);
INSERT INTO health_transition_migration_sequence
 SELECT COALESCE((SELECT seq FROM sqlite_sequence WHERE name='health_transitions'),0),
 (SELECT count(*) FROM sqlite_sequence WHERE name='health_transitions'),
 NOT EXISTS(SELECT 1 FROM sqlite_sequence WHERE name='health_transitions' AND (seq IS NULL OR typeof(seq)!='integer' OR seq NOT BETWEEN 0 AND 9007199254740991));
CREATE TABLE health_transitions_next (
 sequence INTEGER PRIMARY KEY AUTOINCREMENT CHECK(typeof(sequence)='integer' AND sequence BETWEEN 1 AND 9007199254740991),
 event_id TEXT NOT NULL CHECK(length(event_id)=36 AND substr(event_id,9,1)='-' AND substr(event_id,14,1)='-' AND substr(event_id,19,1)='-' AND substr(event_id,24,1)='-' AND length(replace(event_id,'-',''))=32 AND replace(event_id,'-','') NOT GLOB '*[^0-9a-f]*'),
 epoch TEXT NOT NULL CHECK(length(epoch)=36 AND substr(epoch,9,1)='-' AND substr(epoch,14,1)='-' AND substr(epoch,19,1)='-' AND substr(epoch,24,1)='-' AND length(replace(epoch,'-',''))=32 AND replace(epoch,'-','') NOT GLOB '*[^0-9a-f]*'),
 command_id TEXT NOT NULL CHECK(length(command_id)=36 AND substr(command_id,9,1)='-' AND substr(command_id,14,1)='-' AND substr(command_id,19,1)='-' AND substr(command_id,24,1)='-' AND length(replace(command_id,'-',''))=32 AND replace(command_id,'-','') NOT GLOB '*[^0-9a-f]*'),
 -- Zero records historical provenance that was never stored; new writes require 1..3.
 command_attempt INTEGER NOT NULL CHECK(typeof(command_attempt)='integer' AND command_attempt BETWEEN 0 AND 3),
 scope TEXT NOT NULL CHECK(scope IN ('tv','movies','system')),
 check_key TEXT NOT NULL CHECK(length(CAST(check_key AS BLOB)) BETWEEN 1 AND 64 AND check_key NOT GLOB '*[^a-z0-9_]*' AND substr(check_key,1,1) GLOB '[a-z]'),
 kind TEXT NOT NULL CHECK(kind IN ('issue','restored')),
 in_grace INTEGER NOT NULL CHECK(typeof(in_grace)='integer' AND in_grace BETWEEN 0 AND 1),
 created_at INTEGER NOT NULL CHECK(typeof(created_at)='integer' AND created_at BETWEEN 0 AND 9007199254740991),
 severity INTEGER NOT NULL CHECK(typeof(severity)='integer' AND severity BETWEEN 1 AND 3),
 reason TEXT NOT NULL CHECK(typeof(reason)='text' AND length(CAST(reason AS BLOB)) BETWEEN 1 AND 128),
 message TEXT NOT NULL CHECK(typeof(message)='text' AND length(CAST(message AS BLOB)) BETWEEN 1 AND 4096),
 wiki_url TEXT NOT NULL CHECK(typeof(wiki_url)='text' AND length(CAST(wiki_url AS BLOB)) BETWEEN 1 AND 2048),
 compatibility_type TEXT NOT NULL CHECK(typeof(compatibility_type)='text' AND length(CAST(compatibility_type AS BLOB)) BETWEEN 1 AND 128),
 UNIQUE(event_id),
 UNIQUE(command_id,command_attempt,scope,check_key,kind,in_grace)
);
INSERT INTO health_transitions_next(sequence,event_id,epoch,command_id,scope,check_key,kind,in_grace,created_at,severity,reason,message,wiki_url,compatibility_type,command_attempt) SELECT sequence,event_id,epoch,command_id,scope,check_key,kind,in_grace,created_at,severity,reason,message,wiki_url,compatibility_type,0 FROM health_transitions ORDER BY sequence;
-- Empty rings may have no replacement sequence row; never reset an old cursor.
INSERT INTO sqlite_sequence(name,seq)
 SELECT 'health_transitions_next',0 WHERE NOT EXISTS(SELECT 1 FROM sqlite_sequence WHERE name='health_transitions_next');
UPDATE sqlite_sequence SET seq=max(seq,(SELECT high_water FROM health_transition_migration_sequence)) WHERE name='health_transitions_next';
DROP TABLE health_transitions;
ALTER TABLE health_transitions_next RENAME TO health_transitions;
DROP TABLE health_transition_migration_sequence;
CREATE TRIGGER health_transition_attempt BEFORE INSERT ON health_transitions WHEN NEW.command_attempt NOT BETWEEN 1 AND 3 BEGIN SELECT RAISE(ABORT,'health transition requires an attempt'); END;
CREATE TRIGGER health_transition_immutable BEFORE UPDATE ON health_transitions BEGIN SELECT RAISE(ABORT,'health transition is immutable'); END;
-- The diagnostic ring is not a reliable external delivery outbox.
CREATE TRIGGER health_transition_retention AFTER INSERT ON health_transitions BEGIN DELETE FROM health_transitions WHERE sequence IN (SELECT sequence FROM health_transitions ORDER BY sequence DESC LIMIT -1 OFFSET 1024); END;
INSERT INTO health_checks(scope,check_key,startup,scheduled,compatibility_type)VALUES
 ('tv','download_client_communication',1,1,'DownloadClientCheck'),
 ('movies','download_client_communication',1,1,'DownloadClientCheck');
