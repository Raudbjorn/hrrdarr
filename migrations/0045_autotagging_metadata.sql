-- Blank checks use Unicode White_Space (the Rust str::trim set); controls reject C0/C1.
-- Preserve parent identity and incoming FKs while widening the existing runtime authority.
-- ADD/copy/DROP/RENAME was verified with the pinned local libSQL driver and FK ON.
ALTER TABLE movie_metadata ADD COLUMN autotag_runtime_next INTEGER
 CHECK(autotag_runtime_next IS NULL OR (typeof(autotag_runtime_next)='integer' AND autotag_runtime_next BETWEEN 0 AND 10080));
UPDATE movie_metadata SET autotag_runtime_next=runtime;
ALTER TABLE movie_metadata DROP COLUMN runtime;
ALTER TABLE movie_metadata RENAME COLUMN autotag_runtime_next TO runtime;

ALTER TABLE series ADD COLUMN network TEXT CHECK(network IS NULL OR
 (typeof(network)='text' AND length(CAST(network AS BLOB)) BETWEEN 1 AND 1024
 AND length(trim(network,char(9,10,11,12,13,32,133,160,5760,8192,8193,8194,8195,8196,8197,8198,8199,8200,8201,8202,8232,8233,8239,8287,12288)))>0
 AND instr(network,char(0))=0 AND network NOT GLOB '*['||char(1)||'-'||char(31)||char(127)||'-'||char(159)||']*'));
ALTER TABLE movie_metadata ADD COLUMN studio TEXT CHECK(studio IS NULL OR
 (typeof(studio)='text' AND length(CAST(studio AS BLOB)) BETWEEN 1 AND 1024
 AND length(trim(studio,char(9,10,11,12,13,32,133,160,5760,8192,8193,8194,8195,8196,8197,8198,8199,8200,8201,8202,8232,8233,8239,8287,12288)))>0
 AND instr(studio,char(0))=0 AND studio NOT GLOB '*['||char(1)||'-'||char(31)||char(127)||'-'||char(159)||']*'));
ALTER TABLE series ADD COLUMN original_country TEXT CHECK(original_country IS NULL OR
 (typeof(original_country)='text' AND length(CAST(original_country AS BLOB))=3 AND instr(original_country,char(0))=0 AND original_country NOT GLOB '*[^A-Z]*'));
ALTER TABLE series ADD COLUMN status TEXT CHECK(status IS NULL OR status IN ('deleted','continuing','ended','upcoming'));
ALTER TABLE series ADD COLUMN genres_json TEXT CHECK(genres_json IS NULL OR
 (typeof(genres_json)='text' AND length(CAST(genres_json AS BLOB))<=65536 AND CASE WHEN json_valid(genres_json) THEN json_type(genres_json)='array' AND json_array_length(genres_json)<=128 ELSE 0 END));
CREATE TRIGGER autotag_series_genres_json_insert BEFORE INSERT ON series WHEN NEW.genres_json IS NOT NULL
BEGIN
 SELECT CASE WHEN typeof(NEW.genres_json)!='text' OR length(CAST(NEW.genres_json AS BLOB))>65536 OR NOT json_valid(NEW.genres_json) THEN RAISE(ABORT,'invalid autotag metadata array') END;
 SELECT CASE WHEN json_type(NEW.genres_json)!='array' OR json_array_length(NEW.genres_json)>128 THEN RAISE(ABORT,'invalid autotag metadata array') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM json_each(NEW.genres_json) WHERE type!='text'
  OR length(CAST(value AS BLOB)) NOT BETWEEN 1 AND 256
  OR length(trim(value,char(9,10,11,12,13,32,133,160,5760,8192,8193,8194,8195,8196,8197,8198,8199,8200,8201,8202,8232,8233,8239,8287,12288)))=0
  OR instr(value,char(0))!=0 OR value GLOB '*['||char(1)||'-'||char(31)||char(127)||'-'||char(159)||']*')
 THEN RAISE(ABORT,'invalid autotag metadata member') END;
END;
CREATE TRIGGER autotag_series_genres_json_update BEFORE UPDATE OF genres_json ON series WHEN NEW.genres_json IS NOT NULL
BEGIN
 SELECT CASE WHEN typeof(NEW.genres_json)!='text' OR length(CAST(NEW.genres_json AS BLOB))>65536 OR NOT json_valid(NEW.genres_json) THEN RAISE(ABORT,'invalid autotag metadata array') END;
 SELECT CASE WHEN json_type(NEW.genres_json)!='array' OR json_array_length(NEW.genres_json)>128 THEN RAISE(ABORT,'invalid autotag metadata array') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM json_each(NEW.genres_json) WHERE type!='text'
  OR length(CAST(value AS BLOB)) NOT BETWEEN 1 AND 256
  OR length(trim(value,char(9,10,11,12,13,32,133,160,5760,8192,8193,8194,8195,8196,8197,8198,8199,8200,8201,8202,8232,8233,8239,8287,12288)))=0
  OR instr(value,char(0))!=0 OR value GLOB '*['||char(1)||'-'||char(31)||char(127)||'-'||char(159)||']*')
 THEN RAISE(ABORT,'invalid autotag metadata member') END;
END;
ALTER TABLE movie_metadata ADD COLUMN genres_json TEXT CHECK(genres_json IS NULL OR
 (typeof(genres_json)='text' AND length(CAST(genres_json AS BLOB))<=65536 AND CASE WHEN json_valid(genres_json) THEN json_type(genres_json)='array' AND json_array_length(genres_json)<=128 ELSE 0 END));
CREATE TRIGGER autotag_movie_metadata_genres_json_insert BEFORE INSERT ON movie_metadata WHEN NEW.genres_json IS NOT NULL
BEGIN
 SELECT CASE WHEN typeof(NEW.genres_json)!='text' OR length(CAST(NEW.genres_json AS BLOB))>65536 OR NOT json_valid(NEW.genres_json) THEN RAISE(ABORT,'invalid autotag metadata array') END;
 SELECT CASE WHEN json_type(NEW.genres_json)!='array' OR json_array_length(NEW.genres_json)>128 THEN RAISE(ABORT,'invalid autotag metadata array') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM json_each(NEW.genres_json) WHERE type!='text'
  OR length(CAST(value AS BLOB)) NOT BETWEEN 1 AND 256
  OR length(trim(value,char(9,10,11,12,13,32,133,160,5760,8192,8193,8194,8195,8196,8197,8198,8199,8200,8201,8202,8232,8233,8239,8287,12288)))=0
  OR instr(value,char(0))!=0 OR value GLOB '*['||char(1)||'-'||char(31)||char(127)||'-'||char(159)||']*')
 THEN RAISE(ABORT,'invalid autotag metadata member') END;
END;
CREATE TRIGGER autotag_movie_metadata_genres_json_update BEFORE UPDATE OF genres_json ON movie_metadata WHEN NEW.genres_json IS NOT NULL
BEGIN
 SELECT CASE WHEN typeof(NEW.genres_json)!='text' OR length(CAST(NEW.genres_json AS BLOB))>65536 OR NOT json_valid(NEW.genres_json) THEN RAISE(ABORT,'invalid autotag metadata array') END;
 SELECT CASE WHEN json_type(NEW.genres_json)!='array' OR json_array_length(NEW.genres_json)>128 THEN RAISE(ABORT,'invalid autotag metadata array') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM json_each(NEW.genres_json) WHERE type!='text'
  OR length(CAST(value AS BLOB)) NOT BETWEEN 1 AND 256
  OR length(trim(value,char(9,10,11,12,13,32,133,160,5760,8192,8193,8194,8195,8196,8197,8198,8199,8200,8201,8202,8232,8233,8239,8287,12288)))=0
  OR instr(value,char(0))!=0 OR value GLOB '*['||char(1)||'-'||char(31)||char(127)||'-'||char(159)||']*')
 THEN RAISE(ABORT,'invalid autotag metadata member') END;
END;
ALTER TABLE movie_metadata ADD COLUMN keywords_json TEXT CHECK(keywords_json IS NULL OR
 (typeof(keywords_json)='text' AND length(CAST(keywords_json AS BLOB))<=65536 AND CASE WHEN json_valid(keywords_json) THEN json_type(keywords_json)='array' AND json_array_length(keywords_json)<=128 ELSE 0 END));
CREATE TRIGGER autotag_movie_metadata_keywords_json_insert BEFORE INSERT ON movie_metadata WHEN NEW.keywords_json IS NOT NULL
BEGIN
 SELECT CASE WHEN typeof(NEW.keywords_json)!='text' OR length(CAST(NEW.keywords_json AS BLOB))>65536 OR NOT json_valid(NEW.keywords_json) THEN RAISE(ABORT,'invalid autotag metadata array') END;
 SELECT CASE WHEN json_type(NEW.keywords_json)!='array' OR json_array_length(NEW.keywords_json)>128 THEN RAISE(ABORT,'invalid autotag metadata array') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM json_each(NEW.keywords_json) WHERE type!='text'
  OR length(CAST(value AS BLOB)) NOT BETWEEN 1 AND 256
  OR length(trim(value,char(9,10,11,12,13,32,133,160,5760,8192,8193,8194,8195,8196,8197,8198,8199,8200,8201,8202,8232,8233,8239,8287,12288)))=0
  OR instr(value,char(0))!=0 OR value GLOB '*['||char(1)||'-'||char(31)||char(127)||'-'||char(159)||']*')
 THEN RAISE(ABORT,'invalid autotag metadata member') END;
END;
CREATE TRIGGER autotag_movie_metadata_keywords_json_update BEFORE UPDATE OF keywords_json ON movie_metadata WHEN NEW.keywords_json IS NOT NULL
BEGIN
 SELECT CASE WHEN typeof(NEW.keywords_json)!='text' OR length(CAST(NEW.keywords_json AS BLOB))>65536 OR NOT json_valid(NEW.keywords_json) THEN RAISE(ABORT,'invalid autotag metadata array') END;
 SELECT CASE WHEN json_type(NEW.keywords_json)!='array' OR json_array_length(NEW.keywords_json)>128 THEN RAISE(ABORT,'invalid autotag metadata array') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM json_each(NEW.keywords_json) WHERE type!='text'
  OR length(CAST(value AS BLOB)) NOT BETWEEN 1 AND 256
  OR length(trim(value,char(9,10,11,12,13,32,133,160,5760,8192,8193,8194,8195,8196,8197,8198,8199,8200,8201,8202,8232,8233,8239,8287,12288)))=0
  OR instr(value,char(0))!=0 OR value GLOB '*['||char(1)||'-'||char(31)||char(127)||'-'||char(159)||']*')
 THEN RAISE(ABORT,'invalid autotag metadata member') END;
END;
-- NULL facts stay unknown; JSON [] remains a concrete known-empty set.
ALTER TABLE snapshot_imports ADD COLUMN autotag_metadata_version INTEGER NOT NULL DEFAULT 0
 CHECK(typeof(autotag_metadata_version)='integer' AND autotag_metadata_version IN (0,1));
