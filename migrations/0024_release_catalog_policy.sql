-- Nullable catalog facts; no invented eligibility or automation defaults for existing libraries.
ALTER TABLE movie_metadata ADD COLUMN runtime INTEGER CHECK(runtime IS NULL OR (typeof(runtime)='integer' AND runtime BETWEEN 1 AND 10080));
ALTER TABLE movie_metadata ADD COLUMN status TEXT CHECK(status IS NULL OR status IN ('tba','announced','in_cinemas','released','deleted'));
ALTER TABLE movie_metadata ADD COLUMN in_cinemas TEXT CHECK(in_cinemas IS NULL OR length(in_cinemas) BETWEEN 19 AND 29);
ALTER TABLE movie_metadata ADD COLUMN digital_release TEXT CHECK(digital_release IS NULL OR length(digital_release) BETWEEN 19 AND 29);
ALTER TABLE movie_metadata ADD COLUMN physical_release TEXT CHECK(physical_release IS NULL OR length(physical_release) BETWEEN 19 AND 29);
ALTER TABLE movie_metadata ADD COLUMN secondary_year INTEGER CHECK(secondary_year IS NULL OR (typeof(secondary_year)='integer' AND secondary_year BETWEEN 1 AND 9999));
ALTER TABLE movie_metadata ADD COLUMN original_language INTEGER CHECK(original_language IS NULL OR (typeof(original_language)='integer' AND original_language BETWEEN 0 AND 57));
CREATE TABLE movie_alternative_titles (
 metadata_id INTEGER NOT NULL REFERENCES movie_metadata(id) ON DELETE CASCADE,
 title TEXT NOT NULL CHECK(typeof(title)='text' AND length(CAST(title AS BLOB)) BETWEEN 1 AND 1024 AND instr(title,char(0))=0),
 PRIMARY KEY(metadata_id,title)
);
CREATE TRIGGER movie_alternative_title_limit BEFORE INSERT ON movie_alternative_titles
WHEN NOT EXISTS(SELECT 1 FROM movie_alternative_titles WHERE metadata_id=NEW.metadata_id AND title=NEW.title)
 AND (SELECT count(*) FROM movie_alternative_titles WHERE metadata_id=NEW.metadata_id)>=64
BEGIN SELECT RAISE(ABORT,'movie alternate title limit reached'); END;
CREATE TABLE release_delay_policies (
 media_type TEXT PRIMARY KEY NOT NULL CHECK(media_type IN ('tv','movies')),
 torrent_delay_minutes INTEGER NOT NULL CHECK(typeof(torrent_delay_minutes)='integer' AND torrent_delay_minutes BETWEEN 0 AND 10080),
 usenet_delay_minutes INTEGER NOT NULL CHECK(typeof(usenet_delay_minutes)='integer' AND usenet_delay_minutes BETWEEN 0 AND 10080),
 availability_delay_days INTEGER NOT NULL CHECK(typeof(availability_delay_days)='integer' AND availability_delay_days BETWEEN -365 AND 365),
 CHECK(media_type='movies' OR availability_delay_days=0)
);
