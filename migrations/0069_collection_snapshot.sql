-- Reconciles origin/main's collections checkpoint (its published 0052) with this branch's 0066 model.
-- Policy stays on movie_collections (0066). This adds only the ownership markers that let a newer
-- backup replace what an older backup wrote while native edits and natively fetched metadata always
-- win. The published checkpoint already provides the catalog facts (sort title, images, added date,
-- metadata revision, member origin, import exclusions and the snapshot collection version), and its
-- settings and tag tables are replaced by the checkpoint supersede migration: 0066 stores
-- policy inline. Collection removal intents and provider authority remain preserved.
-- 1 once a person or the API edited the policy; a snapshot never overwrites it and it never returns to 0.
ALTER TABLE movie_collections ADD COLUMN local_edit INTEGER NOT NULL DEFAULT 0 CHECK(local_edit IN(0,1));
-- 1 while the current policy was written by a snapshot import; a newer backup may replace it while local_edit is 0.
ALTER TABLE movie_collections ADD COLUMN snapshot_owned INTEGER NOT NULL DEFAULT 0 CHECK(snapshot_owned IN(0,1));
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
-- The published checkpoint already provides the import exclusions and the snapshot
-- collection version, so only the guards this branch's model uses are added here.
CREATE TRIGGER movie_import_exclusions_revision BEFORE UPDATE ON movie_import_exclusions BEGIN
 SELECT CASE WHEN NEW.tmdb_id IS NOT OLD.tmdb_id OR (OLD.local_edit=1 AND NEW.local_edit=0) OR OLD.revision=9007199254740991 OR NEW.revision!=OLD.revision+1 THEN RAISE(ABORT,'collection_intent_revision') END;
END;
CREATE TRIGGER movie_import_exclusions_retain BEFORE DELETE ON movie_import_exclusions WHEN OLD.local_edit=1 BEGIN
 SELECT RAISE(ABORT,'collection_local_intent_retained');
END;
