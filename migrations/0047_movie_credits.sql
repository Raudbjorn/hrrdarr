-- Movie credits belong to catalog metadata (movie_metadata), never to library membership or TV rows.
-- Replacement is owned by the metadata refresh/add transaction; ids stay stable per (metadata, provider credit id).
CREATE TABLE movie_credits (
 id INTEGER PRIMARY KEY,
 metadata_id INTEGER NOT NULL REFERENCES movie_metadata(id) ON DELETE CASCADE,
 credit_tmdb_id TEXT NOT NULL CHECK(typeof(credit_tmdb_id)='text' AND length(CAST(credit_tmdb_id AS BLOB)) BETWEEN 1 AND 64 AND credit_tmdb_id NOT GLOB '*[^A-Za-z0-9_-]*'),
 person_tmdb_id INTEGER NOT NULL CHECK(typeof(person_tmdb_id)='integer' AND person_tmdb_id BETWEEN 1 AND 2147483647),
 person_name TEXT NOT NULL CHECK(typeof(person_name)='text' AND length(CAST(person_name AS BLOB)) BETWEEN 1 AND 512 AND instr(person_name,char(0))=0),
 department TEXT CHECK(department IS NULL OR (typeof(department)='text' AND length(CAST(department AS BLOB)) BETWEEN 1 AND 256 AND instr(department,char(0))=0)),
 job TEXT CHECK(job IS NULL OR (typeof(job)='text' AND length(CAST(job AS BLOB)) BETWEEN 1 AND 256 AND instr(job,char(0))=0)),
 character TEXT CHECK(character IS NULL OR (typeof(character)='text' AND length(CAST(character AS BLOB)) BETWEEN 1 AND 1024 AND instr(character,char(0))=0)),
 credit_order INTEGER NOT NULL CHECK(typeof(credit_order)='integer' AND credit_order BETWEEN 0 AND 100000),
 credit_type TEXT NOT NULL CHECK(credit_type IN ('cast','crew')),
 -- Remote provider references only: [{"cover_type":"headshot","url":"https://..."}]; no local mapping.
 images_json TEXT NOT NULL DEFAULT '[]' CHECK(typeof(images_json)='text' AND json_valid(images_json) AND json_type(images_json)='array' AND json_array_length(images_json)<=8 AND length(CAST(images_json AS BLOB))<=20000),
 UNIQUE(metadata_id,credit_tmdb_id)
);
CREATE INDEX movie_credits_page ON movie_credits(metadata_id,credit_type,credit_order,id);
CREATE TRIGGER movie_credit_limit BEFORE INSERT ON movie_credits
WHEN (SELECT count(*) FROM movie_credits WHERE metadata_id=NEW.metadata_id)>=500
BEGIN SELECT RAISE(ABORT,'movie credit limit reached'); END;
