-- Converge the immutable PR31 lineage with main b34874c without rewriting its history.
-- Delivery progress survives; old system deliveries use the same TV-first domain as their runner.
CREATE TEMP TABLE pr31_delivery_sequence AS SELECT seq FROM sqlite_sequence WHERE name='notification_deliveries';


CREATE TABLE notification_deliveries_aligned (
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
 lease_owner TEXT,
 UNIQUE(event_sequence,notification_id),
 CHECK((state='pending')=(completed_at IS NULL))
);

INSERT INTO notification_deliveries_aligned(id,event_sequence,notification_id,media_type,state,attempts,next_attempt_at,lease_until,last_error,sent_parts,created_at,completed_at,lease_owner)
SELECT d.id,d.event_sequence,d.notification_id,
 CASE WHEN e.scope IN ('tv','movies') THEN e.scope
 WHEN EXISTS(SELECT 1 FROM notification_scopes s WHERE s.notification_id=d.notification_id AND s.media_type='tv') THEN 'tv' ELSE 'movies' END,
 d.state,d.attempts,d.next_attempt_at,d.lease_until,d.last_error,'[]',d.created_at,d.completed_at,NULL
FROM notification_deliveries d JOIN notification_events e ON e.sequence=d.event_sequence;
DROP TABLE notification_deliveries;
ALTER TABLE notification_deliveries_aligned RENAME TO notification_deliveries;
UPDATE sqlite_sequence SET seq=max(seq,coalesce((SELECT seq FROM pr31_delivery_sequence),0)) WHERE name='notification_deliveries';
INSERT INTO sqlite_sequence(name,seq) SELECT 'notification_deliveries',seq FROM pr31_delivery_sequence WHERE NOT EXISTS(SELECT 1 FROM sqlite_sequence WHERE name='notification_deliveries');
DROP TABLE pr31_delivery_sequence;
CREATE INDEX notification_deliveries_due ON notification_deliveries(state,next_attempt_at);

DROP TRIGGER IF EXISTS notification_event_episode_file_delete;
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

DROP TRIGGER IF EXISTS notification_event_movie_file_delete;
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

DROP TRIGGER IF EXISTS notification_event_upgrade_file_delete_rss;
-- Upgrade retirement. Upgrade paths reuse or retain file rows instead of deleting them, so the
-- row-delete triggers cannot observe the replacement. Retirement is the atomic point at which
-- the original leaves the library; emit the documented Upgrade file-delete event here.
CREATE TRIGGER notification_event_upgrade_file_delete_rss AFTER UPDATE OF retirement_state ON rss_candidate_imports
WHEN NEW.retirement_state='quarantined'
 AND OLD.retirement_state IS NOT 'quarantined'
 AND NEW.old_file_json IS NOT NULL
 AND EXISTS(
  SELECT 1 FROM notification_scopes s
  JOIN notifications n ON n.id=s.notification_id
  WHERE n.enabled=1
    AND s.media_type=CASE WHEN NEW.old_episode_file_id IS NOT NULL THEN 'tv' ELSE 'movies' END
    AND (s.on_file_delete=1 OR s.on_file_delete_for_upgrade=1))
BEGIN
 INSERT INTO notification_events(kind,scope,subject_id,payload_json,tag_ids_json,created_at)
 SELECT 'file_delete','tv',f.series_id,
  json_object('episode_file_id',NEW.old_episode_file_id,'path',json_extract(NEW.old_file_json,'$.path'),'reason','Upgrade'),
  (SELECT json_group_array(tag_id) FROM series_tags WHERE series_id=f.series_id),
  CAST(strftime('%s','now') AS INTEGER)
 FROM episode_files f
 WHERE NEW.old_episode_file_id IS NOT NULL AND f.id=NEW.old_episode_file_id;
 INSERT INTO notification_events(kind,scope,subject_id,payload_json,tag_ids_json,created_at)
 SELECT 'file_delete','movies',f.movie_id,
  json_object('movie_file_id',NEW.old_movie_file_id,'path',json_extract(NEW.old_file_json,'$.path'),'reason','Upgrade'),
  (SELECT json_group_array(tag_id) FROM movie_tags WHERE movie_id=f.movie_id),
  CAST(strftime('%s','now') AS INTEGER)
 FROM movie_files f
 WHERE NEW.old_movie_file_id IS NOT NULL AND f.id=NEW.old_movie_file_id;
END;

DROP TRIGGER IF EXISTS notification_event_upgrade_file_delete_scan;
CREATE TRIGGER notification_event_upgrade_file_delete_scan AFTER UPDATE OF retirement_state ON scan_import_replacements
WHEN NEW.retirement_state='quarantined'
 AND OLD.retirement_state IS NOT 'quarantined'
 AND EXISTS(
  SELECT 1 FROM notification_scopes s
  JOIN notifications n ON n.id=s.notification_id
  WHERE n.enabled=1
    AND s.media_type=NEW.media_type
    AND (s.on_file_delete=1 OR s.on_file_delete_for_upgrade=1))
BEGIN
 INSERT INTO notification_events(kind,scope,subject_id,payload_json,tag_ids_json,created_at)
 SELECT 'file_delete','tv',f.series_id,
  json_object('episode_file_id',NEW.old_episode_file_id,'path',json_extract(NEW.old_file_json,'$.path'),'reason','Upgrade'),
  (SELECT json_group_array(tag_id) FROM series_tags WHERE series_id=f.series_id),
  CAST(strftime('%s','now') AS INTEGER)
 FROM episode_files f
 WHERE NEW.media_type='tv' AND NEW.old_episode_file_id IS NOT NULL AND f.id=NEW.old_episode_file_id;
 INSERT INTO notification_events(kind,scope,subject_id,payload_json,tag_ids_json,created_at)
 SELECT 'file_delete','movies',f.movie_id,
  json_object('movie_file_id',NEW.old_movie_file_id,'path',json_extract(NEW.old_file_json,'$.path'),'reason','Upgrade'),
  (SELECT json_group_array(tag_id) FROM movie_tags WHERE movie_id=f.movie_id),
  CAST(strftime('%s','now') AS INTEGER)
 FROM movie_files f
 WHERE NEW.media_type='movies' AND NEW.old_movie_file_id IS NOT NULL AND f.id=NEW.old_movie_file_id;
END;

DROP TRIGGER IF EXISTS notification_event_rename_tv;
-- Rename notifications. One event per renamed series or movie when its command settles, listing the
-- paths it changed; the payload is bounded to the event table's 16 KiB cap (the rest are counted in
-- rename_events, which stays the complete record), so one oversized path set cannot abort the
-- status update that settles the command.
CREATE TRIGGER notification_event_rename_tv AFTER UPDATE OF status ON rename_commands
WHEN NEW.media_type='tv' AND NEW.status IN ('succeeded','failed','cancelled') AND OLD.status NOT IN ('succeeded','failed','cancelled') AND NEW.items_renamed>0
 AND EXISTS(SELECT 1 FROM notification_scopes s JOIN notifications n ON n.id=s.notification_id WHERE n.enabled=1 AND s.media_type='tv' AND s.on_rename=1)
BEGIN
 INSERT INTO notification_events(kind,scope,subject_id,payload_json,created_at)
 SELECT 'rename','tv',series_id,json_object('paths',json_group_array(new_path),'previous_paths',json_group_array(old_path)),CAST(strftime('%s','now') AS INTEGER)
 FROM (SELECT series_id,old_path,new_path FROM (
   SELECT series_id,old_path,new_path,rowid,
    SUM(length(CAST(json_quote(old_path) AS BLOB))+length(CAST(json_quote(new_path) AS BLOB))+8) OVER (PARTITION BY series_id ORDER BY rowid) running
   FROM rename_events WHERE command_id=NEW.id AND media_type='tv')
  WHERE running<=15000 ORDER BY series_id,rowid)
 GROUP BY series_id;
END;

DROP TRIGGER IF EXISTS notification_event_rename_movies;
CREATE TRIGGER notification_event_rename_movies AFTER UPDATE OF status ON rename_commands
WHEN NEW.media_type='movies' AND NEW.status IN ('succeeded','failed','cancelled') AND OLD.status NOT IN ('succeeded','failed','cancelled') AND NEW.items_renamed>0
 AND EXISTS(SELECT 1 FROM notification_scopes s JOIN notifications n ON n.id=s.notification_id WHERE n.enabled=1 AND s.media_type='movies' AND s.on_rename=1)
BEGIN
 INSERT INTO notification_events(kind,scope,subject_id,payload_json,created_at)
 SELECT 'rename','movies',movie_id,json_object('paths',json_group_array(new_path),'previous_paths',json_group_array(old_path)),CAST(strftime('%s','now') AS INTEGER)
 FROM (SELECT movie_id,old_path,new_path FROM (
   SELECT movie_id,old_path,new_path,rowid,
    SUM(length(CAST(json_quote(old_path) AS BLOB))+length(CAST(json_quote(new_path) AS BLOB))+8) OVER (PARTITION BY movie_id ORDER BY rowid) running
   FROM rename_events WHERE command_id=NEW.id AND media_type='movies')
  WHERE running<=15000 ORDER BY movie_id,rowid)
 GROUP BY movie_id;
END;

-- Provider scope shapes for every registered implementation at once.
--
-- Two lanes replaced the same six provider_scope triggers: the torrent RSS feed lane (0068) added
-- the 'torrentrss' branch, and the label-torrent family lane (0069) added the
-- 'deluge','rtorrent','utorrent','flood','aria2','downloadstation' branch. Whichever ran last
-- dropped the other lane's branch, so the trigger set is reinstalled here with both. The registry
-- rows, the minimum_seeders column (0068) and provider_label_categories (0069) are left alone:
-- only the trigger bodies are replaced, and provider_scopes is not rebuilt (a rebuild would
-- cascade-delete scopes, test results, schedules and journals under foreign_keys=ON).
--
-- A torrent RSS feed is an indexer without a search API, so its scope carries neither categories
-- nor priorities and its search flags are off. A label client is a torrent client whose scope is
-- one lowercase label with a queue placement and an add-paused flag in initial_state.

DROP TRIGGER provider_scope_shape_insert;
CREATE TRIGGER provider_scope_shape_insert BEFORE INSERT ON provider_scopes
WHEN NOT ((NEW.implementation IN ('torznab','newznab') AND NEW.categories IS NOT NULL AND NEW.anime_categories IS NOT NULL
    AND NEW.category IS NULL AND NEW.imported_category IS NULL AND NEW.recent_priority IS NULL AND NEW.older_priority IS NULL
    AND ((NEW.media_type='movies' AND json_array_length(NEW.categories)>0 AND json_array_length(NEW.anime_categories)=0)
      OR (NEW.media_type='tv' AND json_array_length(NEW.categories)+json_array_length(NEW.anime_categories)>0)))
  OR (NEW.implementation='torrentrss' AND NEW.categories IS NULL AND NEW.anime_categories IS NULL AND NEW.category IS NULL
    AND NEW.imported_category IS NULL AND NEW.recent_priority IS NULL AND NEW.older_priority IS NULL)
  OR (NEW.implementation='qbittorrent' AND NEW.categories IS NULL AND NEW.anime_categories IS NULL AND NEW.category IS NOT NULL
    AND NEW.recent_priority IN (0,1) AND NEW.older_priority IN (0,1))
  OR (NEW.implementation='sabnzbd' AND NEW.categories IS NULL AND NEW.anime_categories IS NULL AND NEW.category IS NOT NULL
    AND NEW.imported_category IS NULL AND NEW.download_client_id IS NULL
    AND NEW.recent_priority IN (-100,-2,-1,0,1,2) AND NEW.older_priority IN (-100,-2,-1,0,1,2))
  OR (NEW.implementation='transmission' AND NEW.categories IS NULL AND NEW.anime_categories IS NULL
    AND NEW.imported_category IS NULL AND NEW.download_client_id IS NULL
    AND typeof(NEW.recent_priority)='integer' AND NEW.recent_priority IN (0,1)
    AND typeof(NEW.older_priority)='integer' AND NEW.older_priority IN (0,1)
    AND (NEW.category IS NULL OR (typeof(NEW.category)='text' AND length(NEW.category) BETWEEN 1 AND 64
      AND NEW.category NOT GLOB '*[^A-Za-z0-9._-]*' AND NEW.category NOT IN ('.','..'))))
  OR (NEW.implementation='nzbget' AND NEW.categories IS NULL AND NEW.anime_categories IS NULL AND NEW.category IS NOT NULL
    AND NEW.imported_category IS NULL AND NEW.download_client_id IS NULL
    AND typeof(NEW.recent_priority)='integer' AND NEW.recent_priority IN (-100,-2,-1,0,1,2)
    AND typeof(NEW.older_priority)='integer' AND NEW.older_priority IN (-100,-2,-1,0,1,2))
  OR (NEW.implementation IN ('deluge','rtorrent','utorrent','flood','aria2','downloadstation') AND NEW.categories IS NULL AND NEW.anime_categories IS NULL
    AND NEW.imported_category IS NULL AND NEW.download_client_id IS NULL
    AND typeof(NEW.recent_priority)='integer' AND NEW.recent_priority IN (0,1)
    AND typeof(NEW.older_priority)='integer' AND NEW.older_priority IN (0,1)
    AND typeof(NEW.category)='text' AND length(NEW.category) BETWEEN 1 AND 64
    AND NEW.category NOT GLOB '*[^a-z0-9._-]*'))
BEGIN SELECT RAISE(ABORT,'invalid provider scope shape'); END;

DROP TRIGGER provider_scope_shape_update;
CREATE TRIGGER provider_scope_shape_update BEFORE UPDATE ON provider_scopes
WHEN NOT ((NEW.implementation IN ('torznab','newznab') AND NEW.categories IS NOT NULL AND NEW.anime_categories IS NOT NULL
    AND NEW.category IS NULL AND NEW.imported_category IS NULL AND NEW.recent_priority IS NULL AND NEW.older_priority IS NULL
    AND ((NEW.media_type='movies' AND json_array_length(NEW.categories)>0 AND json_array_length(NEW.anime_categories)=0)
      OR (NEW.media_type='tv' AND json_array_length(NEW.categories)+json_array_length(NEW.anime_categories)>0)))
  OR (NEW.implementation='torrentrss' AND NEW.categories IS NULL AND NEW.anime_categories IS NULL AND NEW.category IS NULL
    AND NEW.imported_category IS NULL AND NEW.recent_priority IS NULL AND NEW.older_priority IS NULL)
  OR (NEW.implementation='qbittorrent' AND NEW.categories IS NULL AND NEW.anime_categories IS NULL AND NEW.category IS NOT NULL
    AND NEW.recent_priority IN (0,1) AND NEW.older_priority IN (0,1))
  OR (NEW.implementation='sabnzbd' AND NEW.categories IS NULL AND NEW.anime_categories IS NULL AND NEW.category IS NOT NULL
    AND NEW.imported_category IS NULL AND NEW.download_client_id IS NULL
    AND NEW.recent_priority IN (-100,-2,-1,0,1,2) AND NEW.older_priority IN (-100,-2,-1,0,1,2))
  OR (NEW.implementation='transmission' AND NEW.categories IS NULL AND NEW.anime_categories IS NULL
    AND NEW.imported_category IS NULL AND NEW.download_client_id IS NULL
    AND typeof(NEW.recent_priority)='integer' AND NEW.recent_priority IN (0,1)
    AND typeof(NEW.older_priority)='integer' AND NEW.older_priority IN (0,1)
    AND (NEW.category IS NULL OR (typeof(NEW.category)='text' AND length(NEW.category) BETWEEN 1 AND 64
      AND NEW.category NOT GLOB '*[^A-Za-z0-9._-]*' AND NEW.category NOT IN ('.','..'))))
  OR (NEW.implementation='nzbget' AND NEW.categories IS NULL AND NEW.anime_categories IS NULL AND NEW.category IS NOT NULL
    AND NEW.imported_category IS NULL AND NEW.download_client_id IS NULL
    AND typeof(NEW.recent_priority)='integer' AND NEW.recent_priority IN (-100,-2,-1,0,1,2)
    AND typeof(NEW.older_priority)='integer' AND NEW.older_priority IN (-100,-2,-1,0,1,2))
  OR (NEW.implementation IN ('deluge','rtorrent','utorrent','flood','aria2','downloadstation') AND NEW.categories IS NULL AND NEW.anime_categories IS NULL
    AND NEW.imported_category IS NULL AND NEW.download_client_id IS NULL
    AND typeof(NEW.recent_priority)='integer' AND NEW.recent_priority IN (0,1)
    AND typeof(NEW.older_priority)='integer' AND NEW.older_priority IN (0,1)
    AND typeof(NEW.category)='text' AND length(NEW.category) BETWEEN 1 AND 64
    AND NEW.category NOT GLOB '*[^a-z0-9._-]*'))
BEGIN SELECT RAISE(ABORT,'invalid provider scope shape'); END;

DROP TRIGGER provider_scope_options_insert;
CREATE TRIGGER provider_scope_options_insert BEFORE INSERT ON provider_scopes
WHEN NOT ((NEW.implementation IN ('torznab','newznab') AND NEW.media_type='tv'
    AND typeof(NEW.anime_standard_format_search)='integer' AND NEW.anime_standard_format_search IN (0,1) AND NEW.remove_year IS NULL)
 OR (NEW.implementation IN ('torznab','newznab') AND NEW.media_type='movies'
    AND typeof(NEW.remove_year)='integer' AND NEW.remove_year IN (0,1) AND NEW.anime_standard_format_search IS NULL)
 OR (NEW.implementation IN ('qbittorrent','sabnzbd','transmission','nzbget','deluge','rtorrent','utorrent','flood','aria2','downloadstation') AND NEW.anime_standard_format_search IS NULL AND NEW.remove_year IS NULL)
 OR (NEW.implementation='torrentrss' AND NEW.anime_standard_format_search IS NULL AND NEW.remove_year IS NULL
    AND NEW.enable_automatic_search=0 AND NEW.enable_interactive_search=0))
BEGIN SELECT RAISE(ABORT,'invalid indexer scope options'); END;

DROP TRIGGER provider_scope_options_update;
CREATE TRIGGER provider_scope_options_update BEFORE UPDATE ON provider_scopes
WHEN NOT ((NEW.implementation IN ('torznab','newznab') AND NEW.media_type='tv'
    AND typeof(NEW.anime_standard_format_search)='integer' AND NEW.anime_standard_format_search IN (0,1) AND NEW.remove_year IS NULL)
 OR (NEW.implementation IN ('torznab','newznab') AND NEW.media_type='movies'
    AND typeof(NEW.remove_year)='integer' AND NEW.remove_year IN (0,1) AND NEW.anime_standard_format_search IS NULL)
 OR (NEW.implementation IN ('qbittorrent','sabnzbd','transmission','nzbget','deluge','rtorrent','utorrent','flood','aria2','downloadstation') AND NEW.anime_standard_format_search IS NULL AND NEW.remove_year IS NULL)
 OR (NEW.implementation='torrentrss' AND NEW.anime_standard_format_search IS NULL AND NEW.remove_year IS NULL
    AND NEW.enable_automatic_search=0 AND NEW.enable_interactive_search=0))
BEGIN SELECT RAISE(ABORT,'invalid indexer scope options'); END;

-- Label clients reuse initial_state for "add paused" (started|stopped); qBittorrent-only layout
-- and piece options stay NULL.
DROP TRIGGER provider_client_options_insert;
CREATE TRIGGER provider_client_options_insert BEFORE INSERT ON provider_scopes
WHEN NOT ((NEW.implementation='qbittorrent'
 AND typeof(NEW.initial_state)='text' AND NEW.initial_state IN ('started','stopped','forced')
 AND typeof(NEW.content_layout)='text' AND NEW.content_layout IN ('default','original','subfolder')
 AND typeof(NEW.sequential_order)='integer' AND NEW.sequential_order IN (0,1)
 AND typeof(NEW.first_last_first)='integer' AND NEW.first_last_first IN (0,1)
 AND typeof(NEW.add_tags)='integer' AND NEW.add_tags IN (0,1) AND (NEW.media_type='tv' OR NEW.add_tags=0))
 OR (NEW.implementation IN ('transmission','nzbget','deluge','rtorrent','utorrent','flood','aria2','downloadstation')
 AND typeof(NEW.initial_state)='text' AND NEW.initial_state IN ('started','stopped')
 AND NEW.content_layout IS NULL AND NEW.sequential_order IS NULL AND NEW.first_last_first IS NULL AND NEW.add_tags IS NULL)
 OR (NEW.implementation IN ('torznab','newznab','sabnzbd','torrentrss') AND NEW.initial_state IS NULL AND NEW.content_layout IS NULL
 AND NEW.sequential_order IS NULL AND NEW.first_last_first IS NULL AND NEW.add_tags IS NULL))
BEGIN SELECT RAISE(ABORT,'invalid client scope options'); END;

DROP TRIGGER provider_client_options_update;
CREATE TRIGGER provider_client_options_update BEFORE UPDATE ON provider_scopes
WHEN NOT ((NEW.implementation='qbittorrent'
 AND typeof(NEW.initial_state)='text' AND NEW.initial_state IN ('started','stopped','forced')
 AND typeof(NEW.content_layout)='text' AND NEW.content_layout IN ('default','original','subfolder')
 AND typeof(NEW.sequential_order)='integer' AND NEW.sequential_order IN (0,1)
 AND typeof(NEW.first_last_first)='integer' AND NEW.first_last_first IN (0,1)
 AND typeof(NEW.add_tags)='integer' AND NEW.add_tags IN (0,1) AND (NEW.media_type='tv' OR NEW.add_tags=0))
 OR (NEW.implementation IN ('transmission','nzbget','deluge','rtorrent','utorrent','flood','aria2','downloadstation')
 AND typeof(NEW.initial_state)='text' AND NEW.initial_state IN ('started','stopped')
 AND NEW.content_layout IS NULL AND NEW.sequential_order IS NULL AND NEW.first_last_first IS NULL AND NEW.add_tags IS NULL)
 OR (NEW.implementation IN ('torznab','newznab','sabnzbd','torrentrss') AND NEW.initial_state IS NULL AND NEW.content_layout IS NULL
 AND NEW.sequential_order IS NULL AND NEW.first_last_first IS NULL AND NEW.add_tags IS NULL))
BEGIN SELECT RAISE(ABORT,'invalid client scope options'); END;


-- These clients use the category as a directory component, not just a remote label.
CREATE TRIGGER directory_client_category_insert BEFORE INSERT ON provider_scopes
WHEN NEW.implementation IN ('aria2','downloadstation') AND NEW.category IN ('.','..')
BEGIN SELECT RAISE(ABORT,'invalid directory client category'); END;

CREATE TRIGGER directory_client_category_update BEFORE UPDATE ON provider_scopes
WHEN NEW.implementation IN ('aria2','downloadstation') AND NEW.category IN ('.','..')
BEGIN SELECT RAISE(ABORT,'invalid directory client category'); END;

UPDATE provider_scopes SET category=category WHERE implementation IN ('aria2','downloadstation') AND category IN ('.','..');

-- Legacy is unknown provenance, not an invented snapshot or successful fetch.
CREATE TABLE movie_collection_settings (
 collection_id INTEGER PRIMARY KEY REFERENCES movie_collections(id) ON DELETE RESTRICT,
 media_type TEXT NOT NULL DEFAULT 'movies' CHECK(media_type='movies'),
 settings_revision INTEGER NOT NULL DEFAULT 1 CHECK(typeof(settings_revision)='integer' AND settings_revision BETWEEN 1 AND 9007199254740991),
 monitored INTEGER NOT NULL DEFAULT 0 CHECK(monitored IN(0,1)),
 root_folder_id INTEGER REFERENCES root_folders(id) ON DELETE RESTRICT,
 quality_profile_id INTEGER,
 minimum_availability TEXT CHECK(minimum_availability IN('tba','announced','in_cinemas','released')),
 search_on_add INTEGER CHECK(search_on_add IN(0,1)),
 local_edit INTEGER NOT NULL DEFAULT 0 CHECK(local_edit IN(0,1)),
 -- Written by a snapshot import; a newer backup may replace it while local_edit stays 0.
 snapshot_owned INTEGER NOT NULL DEFAULT 0 CHECK(snapshot_owned IN(0,1)),
 FOREIGN KEY(quality_profile_id,media_type) REFERENCES quality_profiles(id,media_type) ON DELETE RESTRICT,
 CHECK(monitored=0 OR (root_folder_id IS NOT NULL AND quality_profile_id IS NOT NULL AND minimum_availability IS NOT NULL AND search_on_add IS NOT NULL))
);

-- Carry policy and ownership; incomplete monitored defaults remain saved but inactive.
INSERT INTO movie_collection_settings(collection_id,settings_revision,monitored,root_folder_id,quality_profile_id,minimum_availability,search_on_add,local_edit,snapshot_owned)
SELECT id,coalesce(checkpoint_settings_revision,1),
 CASE WHEN root_folder_id IS NOT NULL AND quality_profile_id IS NOT NULL AND minimum_availability IS NOT NULL THEN monitored ELSE 0 END,
 root_folder_id,quality_profile_id,minimum_availability,search_on_add,local_edit,snapshot_owned FROM movie_collections
WHERE checkpoint_settings_revision IS NOT NULL OR monitored!=0 OR search_on_add!=0 OR root_folder_id IS NOT NULL OR quality_profile_id IS NOT NULL OR minimum_availability IS NOT NULL OR local_edit!=0 OR snapshot_owned!=0
 OR EXISTS(SELECT 1 FROM movie_collection_tags t WHERE t.collection_id=movie_collections.id);

-- Old main upgrades capture absent search policy before the immutable supersede step.
-- Databases already at PR31 version 82 have irreversibly normalized NULL to false;
-- retain their recorded false value rather than inventing the lost source provenance.
CREATE TEMP TABLE IF NOT EXISTS pr31_absent_search_on_add(collection_id INTEGER PRIMARY KEY);
UPDATE movie_collection_settings SET search_on_add=NULL WHERE collection_id IN (SELECT collection_id FROM pr31_absent_search_on_add);
DROP TABLE pr31_absent_search_on_add;
CREATE TABLE movie_collection_tags_authority (
 collection_id INTEGER NOT NULL REFERENCES movie_collection_settings(collection_id) ON DELETE RESTRICT,
 tag_id INTEGER NOT NULL,
 media_type TEXT NOT NULL DEFAULT 'movies' CHECK(media_type='movies'),
 PRIMARY KEY(collection_id,tag_id),
 FOREIGN KEY(tag_id,media_type) REFERENCES tags(id,media_type) ON DELETE RESTRICT
);

INSERT INTO movie_collection_tags_authority SELECT * FROM movie_collection_tags;
DROP TABLE movie_collection_tags;
ALTER TABLE movie_collection_tags_authority RENAME TO movie_collection_tags;
CREATE INDEX movie_collection_tags_tag ON movie_collection_tags(tag_id,collection_id);
-- Settings is now authoritative; keep metadata and historical ownership facts untouched.
UPDATE movie_collections SET monitored=0,search_on_add=0,root_folder_id=NULL,quality_profile_id=NULL,minimum_availability=NULL;

DROP TRIGGER IF EXISTS collection_profile_delete;
CREATE TRIGGER collection_profile_delete BEFORE DELETE ON quality_profiles
WHEN EXISTS(SELECT 1 FROM movie_collection_settings WHERE quality_profile_id=OLD.id)
BEGIN SELECT RAISE(ABORT,'collection_profile_in_use'); END;

DROP TRIGGER IF EXISTS collection_profile_identity_update;
CREATE TRIGGER collection_profile_identity_update BEFORE UPDATE OF id,media_type ON quality_profiles
WHEN (NEW.id!=OLD.id OR NEW.media_type!='movies')
 AND EXISTS(SELECT 1 FROM movie_collection_settings WHERE quality_profile_id=OLD.id)
BEGIN SELECT RAISE(ABORT,'collection_profile_in_use'); END;

DROP TRIGGER IF EXISTS collections_identity_insert;

CREATE TRIGGER collections_identity_insert BEFORE INSERT ON movie_collections BEGIN
 SELECT CASE WHEN NEW.id NOT BETWEEN 1 AND 9007199254740991 AND NEW.id!=-1 THEN RAISE(ABORT,'collection_identity') END;
 SELECT CASE WHEN NEW.tmdb_id IS NOT NULL AND (typeof(NEW.tmdb_id)!='integer' OR NEW.tmdb_id NOT BETWEEN 1 AND 9007199254740991) THEN RAISE(ABORT,'collection_identity') END;
 SELECT CASE WHEN typeof(NEW.title)!='text' OR length(CAST(NEW.title AS BLOB)) NOT BETWEEN 1 AND 1024 OR instr(NEW.title,char(0))!=0 THEN RAISE(ABORT,'collection_title') END;
END;

DROP TRIGGER IF EXISTS collections_identity_update;
CREATE TRIGGER collections_identity_update BEFORE UPDATE OF id,tmdb_id,title ON movie_collections BEGIN
 SELECT CASE WHEN NEW.id IS NOT OLD.id OR NEW.tmdb_id IS NOT OLD.tmdb_id THEN RAISE(ABORT,'collection_identity_immutable') END;
 SELECT CASE WHEN NEW.title IS NOT OLD.title AND (typeof(NEW.title)!='text' OR length(CAST(NEW.title AS BLOB)) NOT BETWEEN 1 AND 1024 OR instr(NEW.title,char(0))!=0) THEN RAISE(ABORT,'collection_title') END;
END;

DROP TRIGGER IF EXISTS collections_generated_id;
CREATE TRIGGER collections_generated_id AFTER INSERT ON movie_collections WHEN NEW.id NOT BETWEEN 1 AND 9007199254740991 BEGIN
 SELECT RAISE(ABORT,'collection_identity');
END;

DROP TRIGGER IF EXISTS collections_sync_insert;
-- Existing last_info_sync uses the historical loose CHECK; fence newly written timestamps.
CREATE TRIGGER collections_sync_insert BEFORE INSERT ON movie_collections
WHEN NEW.last_info_sync IS NOT NULL AND (typeof(NEW.last_info_sync)!='text' OR length(CAST(NEW.last_info_sync AS BLOB))!=20 OR NEW.last_info_sync NOT GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9]Z' OR substr(NEW.last_info_sync,1,4) NOT BETWEEN '0001' AND '9999' OR strftime('%Y-%m-%dT%H:%M:%SZ',julianday(NEW.last_info_sync)) IS NOT NEW.last_info_sync)
BEGIN SELECT RAISE(ABORT,'collection_sync_date'); END;

DROP TRIGGER IF EXISTS collections_sync_update;
CREATE TRIGGER collections_sync_update BEFORE UPDATE OF last_info_sync ON movie_collections
WHEN NEW.last_info_sync IS NOT NULL AND (typeof(NEW.last_info_sync)!='text' OR length(CAST(NEW.last_info_sync AS BLOB))!=20 OR NEW.last_info_sync NOT GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9]Z' OR substr(NEW.last_info_sync,1,4) NOT BETWEEN '0001' AND '9999' OR strftime('%Y-%m-%dT%H:%M:%SZ',julianday(NEW.last_info_sync)) IS NOT NEW.last_info_sync)
BEGIN SELECT RAISE(ABORT,'collection_sync_date'); END;

DROP TRIGGER IF EXISTS collections_metadata_revision;
CREATE TRIGGER collections_metadata_revision BEFORE UPDATE ON movie_collections BEGIN
 SELECT CASE WHEN NEW.metadata_revision IS NOT OLD.metadata_revision AND (OLD.metadata_revision=9007199254740991 OR NEW.metadata_revision!=OLD.metadata_revision+1) THEN RAISE(ABORT,'collection_revision') END;
 SELECT CASE WHEN (NEW.added IS NOT OLD.added OR NEW.title IS NOT OLD.title OR NEW.sort_title IS NOT OLD.sort_title OR NEW.overview IS NOT OLD.overview OR NEW.images_json IS NOT OLD.images_json OR NEW.last_info_sync IS NOT OLD.last_info_sync OR NEW.graph_complete IS NOT OLD.graph_complete) AND (OLD.metadata_revision=9007199254740991 OR NEW.metadata_revision!=OLD.metadata_revision+1) THEN RAISE(ABORT,'collection_revision') END;
END;

DROP TRIGGER IF EXISTS collections_images_insert;
CREATE TRIGGER collections_images_insert BEFORE INSERT ON movie_collections WHEN NEW.images_json IS NOT NULL BEGIN
 SELECT CASE WHEN EXISTS(SELECT 1 FROM json_each(NEW.images_json) e WHERE e.type!='object'
  OR (SELECT count(*) FROM json_each(e.value))!=2
  OR EXISTS(SELECT 1 FROM json_each(e.value) f WHERE f.key NOT IN('cover_type','source_url'))
  OR json_type(e.value,'$.cover_type') IS NOT 'text'
  OR length(CAST(json_extract(e.value,'$.cover_type') AS BLOB)) NOT BETWEEN 1 AND 64
  OR length(trim(json_extract(e.value,'$.cover_type')))=0
  OR json_extract(e.value,'$.cover_type') GLOB '*['||char(1)||'-'||char(31)||char(127)||']*'
  OR instr(json_extract(e.value,'$.cover_type'),char(0))!=0
  OR json_type(e.value,'$.source_url') IS NOT 'text'
  OR length(CAST(json_extract(e.value,'$.source_url') AS BLOB)) NOT BETWEEN 1 AND 2048
  OR length(trim(json_extract(e.value,'$.source_url')))=0
  OR instr(json_extract(e.value,'$.source_url'),char(0))!=0
  OR json_extract(e.value,'$.source_url') GLOB '*['||char(1)||'-'||char(31)||char(127)||']*')
 THEN RAISE(ABORT,'collection_images') END;
END;

DROP TRIGGER IF EXISTS collections_images_update;
CREATE TRIGGER collections_images_update BEFORE UPDATE OF images_json ON movie_collections WHEN NEW.images_json IS NOT NULL BEGIN
 SELECT CASE WHEN EXISTS(SELECT 1 FROM json_each(NEW.images_json) e WHERE e.type!='object'
  OR (SELECT count(*) FROM json_each(e.value))!=2
  OR EXISTS(SELECT 1 FROM json_each(e.value) f WHERE f.key NOT IN('cover_type','source_url'))
  OR json_type(e.value,'$.cover_type') IS NOT 'text'
  OR length(CAST(json_extract(e.value,'$.cover_type') AS BLOB)) NOT BETWEEN 1 AND 64
  OR length(trim(json_extract(e.value,'$.cover_type')))=0
  OR json_extract(e.value,'$.cover_type') GLOB '*['||char(1)||'-'||char(31)||char(127)||']*'
  OR instr(json_extract(e.value,'$.cover_type'),char(0))!=0
  OR json_type(e.value,'$.source_url') IS NOT 'text'
  OR length(CAST(json_extract(e.value,'$.source_url') AS BLOB)) NOT BETWEEN 1 AND 2048
  OR length(trim(json_extract(e.value,'$.source_url')))=0
  OR instr(json_extract(e.value,'$.source_url'),char(0))!=0
  OR json_extract(e.value,'$.source_url') GLOB '*['||char(1)||'-'||char(31)||char(127)||']*')
 THEN RAISE(ABORT,'collection_images') END;
END;

DROP TRIGGER IF EXISTS collection_settings_root_insert;
CREATE TRIGGER collection_settings_root_insert BEFORE INSERT ON movie_collection_settings WHEN NEW.root_folder_id IS NOT NULL BEGIN
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM root_folders WHERE id=NEW.root_folder_id AND media_type='movies') THEN RAISE(ABORT,'collection_root_domain') END;
END;

DROP TRIGGER IF EXISTS collection_settings_root_update;
CREATE TRIGGER collection_settings_root_update BEFORE UPDATE OF root_folder_id ON movie_collection_settings WHEN NEW.root_folder_id IS NOT NULL BEGIN
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM root_folders WHERE id=NEW.root_folder_id AND media_type='movies') THEN RAISE(ABORT,'collection_root_domain') END;
END;

DROP TRIGGER IF EXISTS collection_root_domain_update;
CREATE TRIGGER collection_root_domain_update BEFORE UPDATE OF media_type ON root_folders WHEN NEW.media_type!='movies' AND EXISTS(SELECT 1 FROM movie_collection_settings WHERE root_folder_id=OLD.id) BEGIN
 SELECT RAISE(ABORT,'collection_root_domain');
END;

DROP TRIGGER IF EXISTS collection_settings_revision;
CREATE TRIGGER collection_settings_revision BEFORE UPDATE ON movie_collection_settings BEGIN
 SELECT CASE WHEN NEW.collection_id IS NOT OLD.collection_id OR (OLD.local_edit=1 AND NEW.local_edit=0) THEN RAISE(ABORT,'collection_settings_identity') END;
 SELECT CASE WHEN (NEW.settings_revision IS NOT OLD.settings_revision OR NEW.monitored IS NOT OLD.monitored OR NEW.root_folder_id IS NOT OLD.root_folder_id OR NEW.quality_profile_id IS NOT OLD.quality_profile_id OR NEW.minimum_availability IS NOT OLD.minimum_availability OR NEW.search_on_add IS NOT OLD.search_on_add OR NEW.local_edit IS NOT OLD.local_edit) AND (OLD.settings_revision=9007199254740991 OR NEW.settings_revision!=OLD.settings_revision+1) THEN RAISE(ABORT,'collection_settings_revision') END;
END;

DROP TRIGGER IF EXISTS collection_tags_insert;
CREATE TRIGGER collection_tags_insert BEFORE INSERT ON movie_collection_tags BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM movie_collection_tags WHERE collection_id=NEW.collection_id)>=200 THEN RAISE(ABORT,'collection_tag_limit') END;
END;

DROP TRIGGER IF EXISTS collection_tags_update;
CREATE TRIGGER collection_tags_update BEFORE UPDATE ON movie_collection_tags BEGIN
 SELECT RAISE(ABORT,'collection_tag_replace');
END;

DROP TRIGGER IF EXISTS collection_tags_revision_insert;
CREATE TRIGGER collection_tags_revision_insert AFTER INSERT ON movie_collection_tags BEGIN
 UPDATE movie_collection_settings SET settings_revision=settings_revision+1 WHERE collection_id=NEW.collection_id;
END;

DROP TRIGGER IF EXISTS collection_tags_revision_delete;
CREATE TRIGGER collection_tags_revision_delete AFTER DELETE ON movie_collection_tags BEGIN
 UPDATE movie_collection_settings SET settings_revision=settings_revision+1 WHERE collection_id=OLD.collection_id;
END;

DROP TRIGGER IF EXISTS collection_members_insert;
CREATE TRIGGER collection_members_insert BEFORE INSERT ON movie_collection_members BEGIN
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM movie_collection_members WHERE collection_id=NEW.collection_id AND metadata_id=NEW.metadata_id) AND ((SELECT count(*) FROM movie_collection_members WHERE collection_id=NEW.collection_id)>=1000 OR (SELECT count(*) FROM movie_collection_members)>=100000) THEN RAISE(ABORT,'collection_member_limit') END;
 SELECT CASE WHEN NEW.metadata_revision!=(SELECT metadata_revision FROM movie_collections WHERE id=NEW.collection_id) THEN RAISE(ABORT,'collection_member_revision') END;
END;

DROP TRIGGER IF EXISTS collection_members_update;
CREATE TRIGGER collection_members_update BEFORE UPDATE ON movie_collection_members BEGIN
 SELECT CASE WHEN NEW.collection_id IS NOT OLD.collection_id OR NEW.metadata_id IS NOT OLD.metadata_id THEN RAISE(ABORT,'collection_member_identity') END;
 SELECT CASE WHEN NEW.metadata_revision!=(SELECT metadata_revision FROM movie_collections WHERE id=NEW.collection_id) THEN RAISE(ABORT,'collection_member_revision') END;
END;

DROP TRIGGER IF EXISTS movie_collection_intents_revision;
CREATE TRIGGER movie_collection_intents_revision BEFORE UPDATE ON movie_collection_intents BEGIN
 SELECT CASE WHEN NEW.tmdb_id IS NOT OLD.tmdb_id OR (OLD.local_edit=1 AND NEW.local_edit=0) OR OLD.revision=9007199254740991 OR NEW.revision!=OLD.revision+1 THEN RAISE(ABORT,'collection_intent_revision') END;
END;

DROP TRIGGER IF EXISTS movie_collection_intents_retain;
CREATE TRIGGER movie_collection_intents_retain BEFORE DELETE ON movie_collection_intents WHEN OLD.local_edit=1 BEGIN
 SELECT RAISE(ABORT,'collection_local_intent_retained');
END;

DROP TRIGGER IF EXISTS movie_import_exclusions_revision;
CREATE TRIGGER movie_import_exclusions_revision BEFORE UPDATE ON movie_import_exclusions BEGIN
 SELECT CASE WHEN NEW.tmdb_id IS NOT OLD.tmdb_id OR (OLD.local_edit=1 AND NEW.local_edit=0) OR OLD.revision=9007199254740991 OR NEW.revision!=OLD.revision+1 THEN RAISE(ABORT,'collection_intent_revision') END;
END;

DROP TRIGGER IF EXISTS movie_import_exclusions_retain;
CREATE TRIGGER movie_import_exclusions_retain BEFORE DELETE ON movie_import_exclusions WHEN OLD.local_edit=1 BEGIN
 SELECT RAISE(ABORT,'collection_local_intent_retained');
END;
