-- Notifications (Sonarr "Connections"): a registry of outbound notification targets, per-domain
-- event-trigger flags and tag scopes, plus a durable bounded event outbox and per-notification
-- delivery journal. Events are captured by AFTER/BEFORE triggers on the tables that the real
-- grab, import, delete and health paths already write, so an event commits (or rolls back) atomically
-- with the state change that caused it. The implementation name is validated in Rust, so new
-- providers need no migration.
CREATE TABLE notifications (
 id TEXT PRIMARY KEY NOT NULL CHECK(length(id)=36 AND substr(id,9,1)='-' AND substr(id,14,1)='-' AND substr(id,19,1)='-' AND substr(id,24,1)='-' AND length(replace(id,'-',''))=32 AND replace(id,'-','') NOT GLOB '*[^0-9a-f]*'),
 name TEXT NOT NULL CHECK(typeof(name)='text' AND length(CAST(name AS BLOB)) BETWEEN 1 AND 128 AND name=trim(name)),
 implementation TEXT NOT NULL CHECK(typeof(implementation)='text' AND length(implementation) BETWEEN 1 AND 32 AND implementation NOT GLOB '*[^a-z0-9_]*'),
 enabled INTEGER NOT NULL DEFAULT 1 CHECK(typeof(enabled)='integer' AND enabled IN (0,1)),
 revision INTEGER NOT NULL DEFAULT 1 CHECK(typeof(revision)='integer' AND revision BETWEEN 1 AND 9007199254740991),
 settings_json TEXT NOT NULL CHECK(typeof(settings_json)='text' AND length(CAST(settings_json AS BLOB))<=16384 AND json_valid(settings_json) AND json_type(settings_json)='object'),
 -- AEAD envelope over the secret settings object; NULL means no secret is configured.
 -- Names (never values) of the secret fields that are set, so listing needs no key.
 secret_names TEXT NOT NULL DEFAULT '[]' CHECK(typeof(secret_names)='text' AND length(secret_names)<=2048 AND json_valid(secret_names) AND json_type(secret_names)='array'),
 credentials BLOB CHECK(credentials IS NULL OR (typeof(credentials)='blob' AND length(credentials) BETWEEN 29 AND 16384)),
 created_at INTEGER NOT NULL CHECK(typeof(created_at)='integer' AND created_at BETWEEN 0 AND 9007199254740991),
 updated_at INTEGER NOT NULL CHECK(typeof(updated_at)='integer' AND updated_at BETWEEN 0 AND 9007199254740991)
);
CREATE UNIQUE INDEX notifications_name ON notifications(lower(name));
CREATE TRIGGER notifications_capacity BEFORE INSERT ON notifications
WHEN (SELECT count(*) FROM notifications)>=64 BEGIN SELECT RAISE(ABORT,'notification capacity reached'); END;
CREATE TRIGGER notifications_identity BEFORE UPDATE ON notifications
WHEN NEW.id IS NOT OLD.id OR NEW.implementation IS NOT OLD.implementation OR NEW.created_at IS NOT OLD.created_at OR NEW.revision!=OLD.revision+1
BEGIN SELECT RAISE(ABORT,'notification identity is immutable and revisions advance by one'); END;

-- One row per media domain the notification applies to. The flags are the union of Sonarr's and
-- Radarr's event options; flags that a domain does not have are pinned to 0 so a TV scope can
-- never carry a movie-only trigger (and vice versa).
CREATE TABLE notification_scopes (
 notification_id TEXT NOT NULL REFERENCES notifications(id) ON DELETE CASCADE,
 media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
 on_grab INTEGER NOT NULL DEFAULT 0 CHECK(on_grab IN (0,1)),
 on_import INTEGER NOT NULL DEFAULT 0 CHECK(on_import IN (0,1)),
 on_upgrade INTEGER NOT NULL DEFAULT 0 CHECK(on_upgrade IN (0,1)),
 on_import_complete INTEGER NOT NULL DEFAULT 0 CHECK(on_import_complete IN (0,1) AND (media_type='tv' OR on_import_complete=0)),
 on_rename INTEGER NOT NULL DEFAULT 0 CHECK(on_rename IN (0,1)),
 on_add INTEGER NOT NULL DEFAULT 0 CHECK(on_add IN (0,1)),
 on_delete INTEGER NOT NULL DEFAULT 0 CHECK(on_delete IN (0,1)),
 on_file_delete INTEGER NOT NULL DEFAULT 0 CHECK(on_file_delete IN (0,1)),
 on_file_delete_for_upgrade INTEGER NOT NULL DEFAULT 0 CHECK(on_file_delete_for_upgrade IN (0,1)),
 on_health_issue INTEGER NOT NULL DEFAULT 0 CHECK(on_health_issue IN (0,1)),
 on_health_restored INTEGER NOT NULL DEFAULT 0 CHECK(on_health_restored IN (0,1)),
 include_health_warnings INTEGER NOT NULL DEFAULT 0 CHECK(include_health_warnings IN (0,1)),
 on_application_update INTEGER NOT NULL DEFAULT 0 CHECK(on_application_update IN (0,1)),
 on_manual_interaction INTEGER NOT NULL DEFAULT 0 CHECK(on_manual_interaction IN (0,1)),
 PRIMARY KEY(notification_id,media_type)
);
CREATE TABLE notification_tags (
 notification_id TEXT NOT NULL,
 media_type TEXT NOT NULL,
 tag_id INTEGER NOT NULL,
 PRIMARY KEY(notification_id,media_type,tag_id),
 FOREIGN KEY(notification_id,media_type) REFERENCES notification_scopes(notification_id,media_type) ON DELETE CASCADE,
 FOREIGN KEY(tag_id,media_type) REFERENCES tags(id,media_type)
);
CREATE INDEX notification_tags_tag ON notification_tags(tag_id);
CREATE TRIGGER notification_tag_capacity BEFORE INSERT ON notification_tags
WHEN (SELECT count(*) FROM notification_tags WHERE notification_id=NEW.notification_id AND media_type=NEW.media_type)>=200
BEGIN SELECT RAISE(ABORT,'notification tag capacity reached'); END;

-- Durable event outbox. Bounded: only events some enabled notification could consume are
-- recorded, and the oldest are discarded past the cap (their deliveries cascade).
CREATE TABLE notification_events (
 sequence INTEGER PRIMARY KEY AUTOINCREMENT CHECK(typeof(sequence)='integer' AND sequence BETWEEN 1 AND 9007199254740991),
 kind TEXT NOT NULL CHECK(kind IN ('grab','import','rename','add','delete','file_delete','health_issue','health_restored','application_update','manual_interaction')),
 scope TEXT NOT NULL CHECK(scope IN ('tv','movies','system')),
 -- series id for tv, movie id for movies; NULL for system events.
 subject_id INTEGER CHECK(subject_id IS NULL OR (typeof(subject_id)='integer' AND subject_id BETWEEN 1 AND 9007199254740991)),
 payload_json TEXT NOT NULL CHECK(typeof(payload_json)='text' AND length(CAST(payload_json AS BLOB))<=16384 AND json_valid(payload_json) AND json_type(payload_json)='object'),
 -- Tag snapshot for events whose subject row disappears (deletes); NULL means read live tags.
 tag_ids_json TEXT CHECK(tag_ids_json IS NULL OR (json_valid(tag_ids_json) AND json_type(tag_ids_json)='array')),
 created_at INTEGER NOT NULL CHECK(typeof(created_at)='integer' AND created_at BETWEEN 0 AND 9007199254740991),
 fanned_out INTEGER NOT NULL DEFAULT 0 CHECK(fanned_out IN (0,1)),
 CHECK(scope!='system' OR subject_id IS NULL),
 CHECK(subject_id IS NOT NULL OR kind IN ('health_issue','health_restored','application_update'))
);
CREATE INDEX notification_events_pending ON notification_events(fanned_out,sequence);
CREATE TRIGGER notification_events_retention AFTER INSERT ON notification_events
BEGIN DELETE FROM notification_events WHERE sequence IN (SELECT sequence FROM notification_events ORDER BY sequence DESC LIMIT -1 OFFSET 2048); END;

CREATE TABLE notification_deliveries (
 id INTEGER PRIMARY KEY AUTOINCREMENT CHECK(typeof(id)='integer' AND id BETWEEN 1 AND 9007199254740991),
 event_sequence INTEGER NOT NULL REFERENCES notification_events(sequence) ON DELETE CASCADE,
 notification_id TEXT NOT NULL REFERENCES notifications(id) ON DELETE CASCADE,
 -- The scope that matched at fan-out; a system event is rendered for this domain.
 media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
 state TEXT NOT NULL DEFAULT 'pending' CHECK(state IN ('pending','delivered','failed')),
 attempts INTEGER NOT NULL DEFAULT 0 CHECK(typeof(attempts)='integer' AND attempts BETWEEN 0 AND 16),
 next_attempt_at INTEGER NOT NULL CHECK(typeof(next_attempt_at)='integer' AND next_attempt_at BETWEEN 0 AND 9007199254740991),
 -- A claimed delivery that was never resolved (process crash) becomes claimable again here.
 lease_until INTEGER NOT NULL DEFAULT 0 CHECK(typeof(lease_until)='integer' AND lease_until BETWEEN 0 AND 9007199254740991),
 last_error TEXT CHECK(last_error IS NULL OR (length(last_error) BETWEEN 1 AND 64 AND last_error NOT GLOB '*[^a-z0-9_]*')),
 -- Fingerprints of the requests of a multi-request delivery already acknowledged, so a retry
 -- re-sends only the rest.
 sent_parts TEXT NOT NULL DEFAULT '[]' CHECK(typeof(sent_parts)='text' AND length(sent_parts)<=16384 AND json_valid(sent_parts) AND json_type(sent_parts)='array'),
 created_at INTEGER NOT NULL CHECK(typeof(created_at)='integer' AND created_at BETWEEN 0 AND 9007199254740991),
 completed_at INTEGER CHECK(completed_at IS NULL OR (typeof(completed_at)='integer' AND completed_at BETWEEN 0 AND 9007199254740991)),
 UNIQUE(event_sequence,notification_id),
 CHECK((state='pending')=(completed_at IS NULL))
);
CREATE INDEX notification_deliveries_due ON notification_deliveries(state,next_attempt_at);

-- Event capture. Every trigger is gated on an enabled notification that has the matching flag for
-- the event's domain, so an unconfigured install records nothing.
-- Grab: the download client has been observed to hold the release.
CREATE TRIGGER notification_event_grab AFTER UPDATE OF status ON rss_candidates
WHEN NEW.status='observed' AND OLD.status IS NOT 'observed'
 AND EXISTS(SELECT 1 FROM notification_scopes s JOIN notifications n ON n.id=s.notification_id WHERE n.enabled=1 AND s.media_type=NEW.media_type AND s.on_grab=1)
BEGIN
 INSERT INTO notification_events(kind,scope,subject_id,payload_json,created_at)
 VALUES('grab',NEW.media_type,coalesce(NEW.series_id,NEW.movie_id),
  json_object('candidate_id',NEW.id,'release_title',NEW.title,'indexer_id',NEW.indexer_id,'client_id',NEW.client_id),
  CAST(strftime('%s','now') AS INTEGER));
END;
-- Manual interaction: the grab journal escalated a candidate to a person.
CREATE TRIGGER notification_event_manual AFTER UPDATE OF status ON rss_candidates
WHEN NEW.status='needs_attention' AND OLD.status IS NOT 'needs_attention' AND coalesce(NEW.series_id,NEW.movie_id) IS NOT NULL
 AND EXISTS(SELECT 1 FROM notification_scopes s JOIN notifications n ON n.id=s.notification_id WHERE n.enabled=1 AND s.media_type=NEW.media_type AND s.on_manual_interaction=1)
BEGIN
 INSERT INTO notification_events(kind,scope,subject_id,payload_json,created_at)
 VALUES('manual_interaction',NEW.media_type,coalesce(NEW.series_id,NEW.movie_id),
  json_object('candidate_id',NEW.id,'release_title',NEW.title,'indexer_id',NEW.indexer_id,'client_id',NEW.client_id),
  CAST(strftime('%s','now') AS INTEGER));
END;
-- Import / upgrade: the committed import receipt. An upgrade is an import that replaced a file.
CREATE TRIGGER notification_event_import_episode AFTER INSERT ON import_history
WHEN NEW.media_type='episode'
 AND EXISTS(SELECT 1 FROM notification_scopes s JOIN notifications n ON n.id=s.notification_id WHERE n.enabled=1 AND s.media_type='tv' AND s.on_import=1)
BEGIN
 INSERT INTO notification_events(kind,scope,subject_id,payload_json,created_at)
 SELECT 'import','tv',e.series_id,
  json_object('operation_id',NEW.operation_id,'episode_id',NEW.episode_id,'episode_file_id',NEW.episode_file_id,
   'source_path',NEW.source,'path',NEW.destination,'size',NEW.size,
   'is_upgrade',CASE WHEN EXISTS(SELECT 1 FROM rss_candidate_imports r WHERE r.operation_id=NEW.operation_id AND r.old_file_json IS NOT NULL)
     OR EXISTS(SELECT 1 FROM scan_import_replacements r WHERE r.operation_id=NEW.operation_id) THEN 1 ELSE 0 END),
  CAST(strftime('%s','now') AS INTEGER)
 FROM episodes e WHERE e.id=NEW.episode_id;
END;
CREATE TRIGGER notification_event_import_movie AFTER INSERT ON import_history
WHEN NEW.media_type='movie'
 AND EXISTS(SELECT 1 FROM notification_scopes s JOIN notifications n ON n.id=s.notification_id WHERE n.enabled=1 AND s.media_type='movies' AND s.on_import=1)
BEGIN
 INSERT INTO notification_events(kind,scope,subject_id,payload_json,created_at)
 VALUES('import','movies',NEW.movie_id,
  json_object('operation_id',NEW.operation_id,'movie_file_id',NEW.movie_file_id,
   'source_path',NEW.source,'path',NEW.destination,'size',NEW.size,
   'is_upgrade',CASE WHEN EXISTS(SELECT 1 FROM rss_candidate_imports r WHERE r.operation_id=NEW.operation_id AND r.old_file_json IS NOT NULL)
     OR EXISTS(SELECT 1 FROM scan_import_replacements r WHERE r.operation_id=NEW.operation_id) THEN 1 ELSE 0 END),
  CAST(strftime('%s','now') AS INTEGER));
END;
-- Add.
CREATE TRIGGER notification_event_series_add AFTER INSERT ON series
WHEN EXISTS(SELECT 1 FROM notification_scopes s JOIN notifications n ON n.id=s.notification_id WHERE n.enabled=1 AND s.media_type='tv' AND s.on_add=1)
BEGIN
 INSERT INTO notification_events(kind,scope,subject_id,payload_json,created_at)
 VALUES('add','tv',NEW.id,json_object('title',NEW.title,'year',NEW.year,'tvdb_id',NEW.tvdb_id,'path',NEW.path),CAST(strftime('%s','now') AS INTEGER));
END;
CREATE TRIGGER notification_event_movie_add AFTER INSERT ON movies
WHEN EXISTS(SELECT 1 FROM notification_scopes s JOIN notifications n ON n.id=s.notification_id WHERE n.enabled=1 AND s.media_type='movies' AND s.on_add=1)
BEGIN
 INSERT INTO notification_events(kind,scope,subject_id,payload_json,created_at)
 SELECT 'add','movies',NEW.id,json_object('title',d.title,'year',d.year,'tmdb_id',d.tmdb_id,'imdb_id',d.imdb_id,'path',NEW.path),CAST(strftime('%s','now') AS INTEGER)
 FROM movie_metadata d WHERE d.id=NEW.metadata_id;
END;
-- Delete: the subject row is gone by dispatch time, so title, path and tags are snapshotted.
CREATE TRIGGER notification_event_series_delete BEFORE DELETE ON series
WHEN EXISTS(SELECT 1 FROM notification_scopes s JOIN notifications n ON n.id=s.notification_id WHERE n.enabled=1 AND s.media_type='tv' AND s.on_delete=1)
BEGIN
 INSERT INTO notification_events(kind,scope,subject_id,payload_json,tag_ids_json,created_at)
 VALUES('delete','tv',OLD.id,json_object('title',OLD.title,'year',OLD.year,'tvdb_id',OLD.tvdb_id,'path',OLD.path),
  (SELECT json_group_array(tag_id) FROM series_tags WHERE series_id=OLD.id),CAST(strftime('%s','now') AS INTEGER));
END;
CREATE TRIGGER notification_event_movie_delete BEFORE DELETE ON movies
WHEN EXISTS(SELECT 1 FROM notification_scopes s JOIN notifications n ON n.id=s.notification_id WHERE n.enabled=1 AND s.media_type='movies' AND s.on_delete=1)
BEGIN
 INSERT INTO notification_events(kind,scope,subject_id,payload_json,tag_ids_json,created_at)
 SELECT 'delete','movies',OLD.id,json_object('title',d.title,'year',d.year,'tmdb_id',d.tmdb_id,'imdb_id',d.imdb_id,'path',OLD.path),
  (SELECT json_group_array(tag_id) FROM movie_tags WHERE movie_id=OLD.id),CAST(strftime('%s','now') AS INTEGER)
 FROM movie_metadata d WHERE d.id=OLD.metadata_id;
END;
-- File delete. The reason is derived from the journals the deleting paths already keep:
-- an explicit deletion request is Manual, a replaced file is Upgrade, anything else is a
-- file that vanished from disk and was reconciled away. Only an unfinished deletion journal
-- counts: the deleting transaction removes the row while the journal is still admitted or
-- copied, an aborted request kept the file, and file ids can be reused after a completed one.
CREATE TRIGGER notification_event_episode_file_delete BEFORE DELETE ON episode_files
WHEN EXISTS(SELECT 1 FROM notification_scopes s JOIN notifications n ON n.id=s.notification_id WHERE n.enabled=1 AND s.media_type='tv' AND (s.on_file_delete=1 OR s.on_file_delete_for_upgrade=1))
BEGIN
 INSERT INTO notification_events(kind,scope,subject_id,payload_json,tag_ids_json,created_at)
 VALUES('file_delete','tv',OLD.series_id,
  json_object('episode_file_id',OLD.id,'path',OLD.path,
   'reason',CASE WHEN EXISTS(SELECT 1 FROM file_deletions d WHERE d.media_type='tv' AND d.file_id=OLD.id AND d.phase IN ('admitted','copied','committed')) THEN 'Manual'
     WHEN EXISTS(SELECT 1 FROM scan_import_replacements r WHERE r.old_episode_file_id=OLD.id) OR EXISTS(SELECT 1 FROM rss_candidate_imports r WHERE r.old_episode_file_id=OLD.id) THEN 'Upgrade'
     WHEN NOT EXISTS(SELECT 1 FROM episodes e WHERE e.episode_file_id=OLD.id) THEN 'NoLinkedEpisodes'
     ELSE 'MissingFromDisk' END),
  (SELECT json_group_array(tag_id) FROM series_tags WHERE series_id=OLD.series_id),CAST(strftime('%s','now') AS INTEGER));
END;
CREATE TRIGGER notification_event_movie_file_delete BEFORE DELETE ON movie_files
WHEN EXISTS(SELECT 1 FROM notification_scopes s JOIN notifications n ON n.id=s.notification_id WHERE n.enabled=1 AND s.media_type='movies' AND (s.on_file_delete=1 OR s.on_file_delete_for_upgrade=1))
BEGIN
 INSERT INTO notification_events(kind,scope,subject_id,payload_json,tag_ids_json,created_at)
 VALUES('file_delete','movies',OLD.movie_id,
  json_object('movie_file_id',OLD.id,'path',OLD.path,
   'reason',CASE WHEN EXISTS(SELECT 1 FROM file_deletions d WHERE d.media_type='movies' AND d.file_id=OLD.id AND d.phase IN ('admitted','copied','committed')) THEN 'Manual'
     WHEN EXISTS(SELECT 1 FROM scan_import_replacements r WHERE r.old_movie_file_id=OLD.id) OR EXISTS(SELECT 1 FROM rss_candidate_imports r WHERE r.old_movie_file_id=OLD.id) THEN 'Upgrade'
     ELSE 'MissingFromDisk' END),
  (SELECT json_group_array(tag_id) FROM movie_tags WHERE movie_id=OLD.movie_id),CAST(strftime('%s','now') AS INTEGER));
END;
-- A movie replacement keeps the file id and rewrites the path in place (also when the new path equals
-- the old one, as UPDATE OF fires on assignment), so the DELETE trigger above never sees an upgrade.
-- The in-place rewrite of a file with an unfinished replacement is the old bytes being retired.
CREATE TRIGGER notification_event_movie_file_upgrade AFTER UPDATE OF path ON movie_files
WHEN EXISTS(SELECT 1 FROM notification_scopes s JOIN notifications n ON n.id=s.notification_id WHERE n.enabled=1 AND s.media_type='movies' AND (s.on_file_delete=1 OR s.on_file_delete_for_upgrade=1))
 AND (EXISTS(SELECT 1 FROM scan_import_replacements r JOIN import_journal j ON j.operation_id=r.operation_id WHERE r.old_movie_file_id=OLD.id AND j.phase!='complete')
  OR EXISTS(SELECT 1 FROM rss_candidate_imports r JOIN import_journal j ON j.operation_id=r.operation_id WHERE r.old_movie_file_id=OLD.id AND j.phase!='complete'))
BEGIN
 INSERT INTO notification_events(kind,scope,subject_id,payload_json,tag_ids_json,created_at)
 VALUES('file_delete','movies',OLD.movie_id,json_object('movie_file_id',OLD.id,'path',OLD.path,'reason','Upgrade'),
  (SELECT json_group_array(tag_id) FROM movie_tags WHERE movie_id=OLD.movie_id),CAST(strftime('%s','now') AS INTEGER));
END;
-- Health: only transitions outside the startup grace window are real state changes. The health
-- ring keeps 1024 rows and is explicitly not a delivery outbox, so this copies what is needed.
CREATE TRIGGER notification_event_health AFTER INSERT ON health_transitions
WHEN NEW.in_grace=0
 AND EXISTS(SELECT 1 FROM notification_scopes s JOIN notifications n ON n.id=s.notification_id
  WHERE n.enabled=1 AND (NEW.scope='system' OR s.media_type=NEW.scope)
  AND ((NEW.kind='issue' AND s.on_health_issue=1) OR (NEW.kind='restored' AND s.on_health_restored=1)))
BEGIN
 INSERT INTO notification_events(kind,scope,payload_json,created_at)
 VALUES(CASE WHEN NEW.kind='issue' THEN 'health_issue' ELSE 'health_restored' END,NEW.scope,
  json_object('check_key',NEW.check_key,'severity',NEW.severity,'reason',NEW.reason,'message',NEW.message,
   'wiki_url',NEW.wiki_url,'type',NEW.compatibility_type),CAST(strftime('%s','now') AS INTEGER));
END;
