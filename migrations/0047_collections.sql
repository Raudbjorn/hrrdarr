-- Catalog facts remain independent from library ownership and local defaults.
-- Existing nullable identities and multiple memberships are preserved verbatim.
ALTER TABLE movie_collections ADD COLUMN sort_title TEXT CHECK(sort_title IS NULL OR (typeof(sort_title)='text' AND length(CAST(sort_title AS BLOB))<=1024 AND instr(sort_title,char(0))=0));
ALTER TABLE movie_collections ADD COLUMN overview TEXT CHECK(overview IS NULL OR (typeof(overview)='text' AND length(CAST(overview AS BLOB))<=65536 AND instr(overview,char(0))=0));
ALTER TABLE movie_collections ADD COLUMN images_json TEXT CHECK(images_json IS NULL OR (typeof(images_json)='text' AND length(CAST(images_json AS BLOB))<=65536 AND json_valid(images_json) AND json_type(images_json)='array' AND json_array_length(images_json)<=32));
ALTER TABLE movie_collections ADD COLUMN metadata_revision INTEGER NOT NULL DEFAULT 0 CHECK(typeof(metadata_revision)='integer' AND metadata_revision BETWEEN 0 AND 9007199254740991);
ALTER TABLE movie_collections ADD COLUMN graph_complete INTEGER NOT NULL DEFAULT 0 CHECK(graph_complete IN(0,1));
ALTER TABLE movie_collections ADD COLUMN added TEXT CHECK(added IS NULL OR (typeof(added)='text' AND length(CAST(added AS BLOB))=20 AND added GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9]Z' AND substr(added,1,4) BETWEEN '0001' AND '9999' AND strftime('%Y-%m-%dT%H:%M:%SZ',julianday(added)) IS added));
ALTER TABLE movie_collections ADD COLUMN last_info_sync TEXT CHECK(last_info_sync IS NULL OR (typeof(last_info_sync)='text' AND length(CAST(last_info_sync AS BLOB))=20 AND last_info_sync GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9]Z' AND substr(last_info_sync,1,4) BETWEEN '0001' AND '9999' AND strftime('%Y-%m-%dT%H:%M:%SZ',julianday(last_info_sync)) IS last_info_sync));
ALTER TABLE movie_collection_members ADD COLUMN origin TEXT NOT NULL DEFAULT 'legacy' CHECK(origin IN('legacy','snapshot','discovery','graph'));
ALTER TABLE movie_collection_members ADD COLUMN metadata_revision INTEGER NOT NULL DEFAULT 0 CHECK(typeof(metadata_revision)='integer' AND metadata_revision BETWEEN 0 AND 9007199254740991);
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
 FOREIGN KEY(quality_profile_id,media_type) REFERENCES quality_profiles(id,media_type) ON DELETE RESTRICT,
 CHECK(monitored=0 OR (root_folder_id IS NOT NULL AND quality_profile_id IS NOT NULL AND minimum_availability IS NOT NULL AND search_on_add IS NOT NULL))
);
CREATE TABLE movie_collection_tags (
 collection_id INTEGER NOT NULL REFERENCES movie_collection_settings(collection_id) ON DELETE RESTRICT,
 tag_id INTEGER NOT NULL,
 media_type TEXT NOT NULL DEFAULT 'movies' CHECK(media_type='movies'),
 PRIMARY KEY(collection_id,tag_id),
 FOREIGN KEY(tag_id,media_type) REFERENCES tags(id,media_type) ON DELETE RESTRICT
);
CREATE INDEX movie_collection_tags_tag ON movie_collection_tags(tag_id,collection_id);
CREATE TABLE movie_collection_intents (
 tmdb_id INTEGER PRIMARY KEY CHECK(typeof(tmdb_id)='integer' AND tmdb_id BETWEEN 1 AND 9007199254740991),
 revision INTEGER NOT NULL DEFAULT 1 CHECK(typeof(revision)='integer' AND revision BETWEEN 1 AND 9007199254740991),
 local_edit INTEGER NOT NULL DEFAULT 0 CHECK(local_edit IN(0,1)),
 removed INTEGER NOT NULL DEFAULT 0 CHECK(removed IN(0,1)),
 removal_reason TEXT CHECK(removal_reason IN('user','metadata_missing')),
 CHECK((removed=0 AND removal_reason IS NULL) OR (removed=1 AND removal_reason IS NOT NULL)),
 CHECK(removal_reason IS NOT 'user' OR local_edit=1)
);
-- No library FK: explicit exclusions and local removals survive catalog deletion.
CREATE TABLE movie_import_exclusions (
 tmdb_id INTEGER PRIMARY KEY CHECK(typeof(tmdb_id)='integer' AND tmdb_id BETWEEN 1 AND 9007199254740991),
 title TEXT CHECK(title IS NULL OR (typeof(title)='text' AND length(CAST(title AS BLOB)) BETWEEN 1 AND 1024 AND instr(title,char(0))=0)),
 year INTEGER CHECK(year IS NULL OR (typeof(year)='integer' AND year BETWEEN 1 AND 9999)),
 excluded INTEGER NOT NULL DEFAULT 1 CHECK(excluded IN(0,1)),
 local_edit INTEGER NOT NULL DEFAULT 0 CHECK(local_edit IN(0,1)),
 revision INTEGER NOT NULL DEFAULT 1 CHECK(typeof(revision)='integer' AND revision BETWEEN 1 AND 9007199254740991),
 CHECK(excluded=1 OR local_edit=1)
);
ALTER TABLE snapshot_imports ADD COLUMN collection_version INTEGER NOT NULL DEFAULT 0 CHECK(collection_version IN(0,1));

CREATE TRIGGER collections_identity_insert BEFORE INSERT ON movie_collections BEGIN
 SELECT CASE WHEN NEW.id NOT BETWEEN 1 AND 9007199254740991 AND NEW.id!=-1 THEN RAISE(ABORT,'collection_identity') END;
 SELECT CASE WHEN NEW.tmdb_id IS NOT NULL AND (typeof(NEW.tmdb_id)!='integer' OR NEW.tmdb_id NOT BETWEEN 1 AND 9007199254740991) THEN RAISE(ABORT,'collection_identity') END;
 SELECT CASE WHEN typeof(NEW.title)!='text' OR length(CAST(NEW.title AS BLOB)) NOT BETWEEN 1 AND 1024 OR instr(NEW.title,char(0))!=0 THEN RAISE(ABORT,'collection_title') END;
END;
CREATE TRIGGER collections_identity_update BEFORE UPDATE OF id,tmdb_id,title ON movie_collections BEGIN
 SELECT CASE WHEN NEW.id IS NOT OLD.id OR NEW.tmdb_id IS NOT OLD.tmdb_id THEN RAISE(ABORT,'collection_identity_immutable') END;
 SELECT CASE WHEN NEW.title IS NOT OLD.title AND (typeof(NEW.title)!='text' OR length(CAST(NEW.title AS BLOB)) NOT BETWEEN 1 AND 1024 OR instr(NEW.title,char(0))!=0) THEN RAISE(ABORT,'collection_title') END;
END;
CREATE TRIGGER collections_generated_id AFTER INSERT ON movie_collections WHEN NEW.id NOT BETWEEN 1 AND 9007199254740991 BEGIN
 SELECT RAISE(ABORT,'collection_identity');
END;
CREATE TRIGGER collections_metadata_revision BEFORE UPDATE ON movie_collections BEGIN
 SELECT CASE WHEN NEW.metadata_revision IS NOT OLD.metadata_revision AND (OLD.metadata_revision=9007199254740991 OR NEW.metadata_revision!=OLD.metadata_revision+1) THEN RAISE(ABORT,'collection_revision') END;
 SELECT CASE WHEN (NEW.added IS NOT OLD.added OR NEW.title IS NOT OLD.title OR NEW.sort_title IS NOT OLD.sort_title OR NEW.overview IS NOT OLD.overview OR NEW.images_json IS NOT OLD.images_json OR NEW.last_info_sync IS NOT OLD.last_info_sync OR NEW.graph_complete IS NOT OLD.graph_complete) AND (OLD.metadata_revision=9007199254740991 OR NEW.metadata_revision!=OLD.metadata_revision+1) THEN RAISE(ABORT,'collection_revision') END;
END;
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
CREATE TRIGGER collection_settings_root_insert BEFORE INSERT ON movie_collection_settings WHEN NEW.root_folder_id IS NOT NULL BEGIN
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM root_folders WHERE id=NEW.root_folder_id AND media_type='movies') THEN RAISE(ABORT,'collection_root_domain') END;
END;
CREATE TRIGGER collection_settings_root_update BEFORE UPDATE OF root_folder_id ON movie_collection_settings WHEN NEW.root_folder_id IS NOT NULL BEGIN
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM root_folders WHERE id=NEW.root_folder_id AND media_type='movies') THEN RAISE(ABORT,'collection_root_domain') END;
END;
CREATE TRIGGER collection_root_domain_update BEFORE UPDATE OF media_type ON root_folders WHEN NEW.media_type!='movies' AND EXISTS(SELECT 1 FROM movie_collection_settings WHERE root_folder_id=OLD.id) BEGIN
 SELECT RAISE(ABORT,'collection_root_domain');
END;
CREATE TRIGGER collection_settings_revision BEFORE UPDATE ON movie_collection_settings BEGIN
 SELECT CASE WHEN NEW.collection_id IS NOT OLD.collection_id OR (OLD.local_edit=1 AND NEW.local_edit=0) THEN RAISE(ABORT,'collection_settings_identity') END;
 SELECT CASE WHEN (NEW.settings_revision IS NOT OLD.settings_revision OR NEW.monitored IS NOT OLD.monitored OR NEW.root_folder_id IS NOT OLD.root_folder_id OR NEW.quality_profile_id IS NOT OLD.quality_profile_id OR NEW.minimum_availability IS NOT OLD.minimum_availability OR NEW.search_on_add IS NOT OLD.search_on_add OR NEW.local_edit IS NOT OLD.local_edit) AND (OLD.settings_revision=9007199254740991 OR NEW.settings_revision!=OLD.settings_revision+1) THEN RAISE(ABORT,'collection_settings_revision') END;
END;
CREATE TRIGGER collection_tags_insert BEFORE INSERT ON movie_collection_tags BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM movie_collection_tags WHERE collection_id=NEW.collection_id)>=200 THEN RAISE(ABORT,'collection_tag_limit') END;
END;
CREATE TRIGGER collection_tags_update BEFORE UPDATE ON movie_collection_tags BEGIN
 SELECT RAISE(ABORT,'collection_tag_replace');
END;
CREATE TRIGGER collection_tags_revision_insert AFTER INSERT ON movie_collection_tags BEGIN
 UPDATE movie_collection_settings SET settings_revision=settings_revision+1 WHERE collection_id=NEW.collection_id;
END;
CREATE TRIGGER collection_tags_revision_delete AFTER DELETE ON movie_collection_tags BEGIN
 UPDATE movie_collection_settings SET settings_revision=settings_revision+1 WHERE collection_id=OLD.collection_id;
END;
CREATE TRIGGER collection_members_insert BEFORE INSERT ON movie_collection_members BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM movie_collection_members WHERE collection_id=NEW.collection_id)>=1000 OR (SELECT count(*) FROM movie_collection_members)>=100000 THEN RAISE(ABORT,'collection_member_limit') END;
 SELECT CASE WHEN NEW.metadata_revision!=(SELECT metadata_revision FROM movie_collections WHERE id=NEW.collection_id) THEN RAISE(ABORT,'collection_member_revision') END;
END;
CREATE TRIGGER collection_members_update BEFORE UPDATE ON movie_collection_members BEGIN
 SELECT CASE WHEN NEW.collection_id IS NOT OLD.collection_id OR NEW.metadata_id IS NOT OLD.metadata_id THEN RAISE(ABORT,'collection_member_identity') END;
 SELECT CASE WHEN NEW.metadata_revision!=(SELECT metadata_revision FROM movie_collections WHERE id=NEW.collection_id) THEN RAISE(ABORT,'collection_member_revision') END;
END;
CREATE TRIGGER movie_collection_intents_revision BEFORE UPDATE ON movie_collection_intents BEGIN
 SELECT CASE WHEN NEW.tmdb_id IS NOT OLD.tmdb_id OR (OLD.local_edit=1 AND NEW.local_edit=0) OR OLD.revision=9007199254740991 OR NEW.revision!=OLD.revision+1 THEN RAISE(ABORT,'collection_intent_revision') END;
END;
CREATE TRIGGER movie_collection_intents_retain BEFORE DELETE ON movie_collection_intents WHEN OLD.local_edit=1 BEGIN
 SELECT RAISE(ABORT,'collection_local_intent_retained');
END;
CREATE TRIGGER movie_import_exclusions_revision BEFORE UPDATE ON movie_import_exclusions BEGIN
 SELECT CASE WHEN NEW.tmdb_id IS NOT OLD.tmdb_id OR (OLD.local_edit=1 AND NEW.local_edit=0) OR OLD.revision=9007199254740991 OR NEW.revision!=OLD.revision+1 THEN RAISE(ABORT,'collection_intent_revision') END;
END;
CREATE TRIGGER movie_import_exclusions_retain BEFORE DELETE ON movie_import_exclusions WHEN OLD.local_edit=1 BEGIN
 SELECT RAISE(ABORT,'collection_local_intent_retained');
END;
