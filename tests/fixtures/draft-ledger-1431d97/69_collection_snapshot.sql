-- Reconciles origin/main's collections checkpoint (its 0052/0053) with this branch's 0066 model.
-- Policy stays on movie_collections (0066). This adds only the facts a Radarr snapshot import carries
-- and the ownership markers that let a newer backup replace what an older backup wrote while native
-- edits and natively fetched metadata always win. Origin's separate settings, intent and
-- provider-authority tables are intentionally not created: 0066 stores policy inline, 0059/0052 store
-- download-client tags, and indexer/client backoff already has durable status tables.
ALTER TABLE movie_collections ADD COLUMN sort_title TEXT CHECK(sort_title IS NULL OR (typeof(sort_title)='text' AND length(CAST(sort_title AS BLOB))<=1024 AND instr(sort_title,char(0))=0));
ALTER TABLE movie_collections ADD COLUMN images_json TEXT CHECK(images_json IS NULL OR (typeof(images_json)='text' AND length(CAST(images_json AS BLOB))<=65536 AND json_valid(images_json) AND json_type(images_json)='array' AND json_array_length(images_json)<=32));
ALTER TABLE movie_collections ADD COLUMN added TEXT CHECK(added IS NULL OR (typeof(added)='text' AND length(CAST(added AS BLOB))=20 AND added GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9]Z' AND substr(added,1,4) BETWEEN '0001' AND '9999' AND strftime('%Y-%m-%dT%H:%M:%SZ',julianday(added)) IS added));
-- Counts natively fetched graphs (RefreshCollections) and native title changes; a snapshot never advances it.
ALTER TABLE movie_collections ADD COLUMN metadata_revision INTEGER NOT NULL DEFAULT 0 CHECK(typeof(metadata_revision)='integer' AND metadata_revision BETWEEN 0 AND 9007199254740991);
-- 1 once a person or the API edited the policy; a snapshot never overwrites it and it never returns to 0.
ALTER TABLE movie_collections ADD COLUMN local_edit INTEGER NOT NULL DEFAULT 0 CHECK(local_edit IN(0,1));
-- 1 while the current policy was written by a snapshot import; a newer backup may replace it while local_edit is 0.
ALTER TABLE movie_collections ADD COLUMN snapshot_owned INTEGER NOT NULL DEFAULT 0 CHECK(snapshot_owned IN(0,1));
-- Legacy is unknown provenance, not an invented snapshot or successful fetch.
ALTER TABLE movie_collection_members ADD COLUMN origin TEXT NOT NULL DEFAULT 'legacy' CHECK(origin IN('legacy','snapshot','discovery','graph'));
CREATE TRIGGER collection_local_edit_sticky BEFORE UPDATE OF local_edit ON movie_collections WHEN OLD.local_edit=1 AND NEW.local_edit=0 BEGIN
 SELECT RAISE(ABORT,'collection_local_edit_retained');
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
CREATE TRIGGER movie_import_exclusions_revision BEFORE UPDATE ON movie_import_exclusions BEGIN
 SELECT CASE WHEN NEW.tmdb_id IS NOT OLD.tmdb_id OR (OLD.local_edit=1 AND NEW.local_edit=0) OR OLD.revision=9007199254740991 OR NEW.revision!=OLD.revision+1 THEN RAISE(ABORT,'collection_intent_revision') END;
END;
CREATE TRIGGER movie_import_exclusions_retain BEFORE DELETE ON movie_import_exclusions WHEN OLD.local_edit=1 BEGIN
 SELECT RAISE(ABORT,'collection_local_intent_retained');
END;
