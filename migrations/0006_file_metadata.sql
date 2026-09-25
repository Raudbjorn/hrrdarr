-- Core file ownership remains in separate domain tables. Missing metadata means unknown.
-- RESTRICT deletion deliberately leaves recovery/deletion services responsible for associations.
CREATE TABLE file_metadata (
    id INTEGER PRIMARY KEY,
    media_type TEXT NOT NULL CHECK (media_type IN ('tv','movies')),
    episode_file_id INTEGER UNIQUE REFERENCES episode_files(id) ON DELETE RESTRICT,
    movie_file_id INTEGER UNIQUE REFERENCES movie_files(id) ON DELETE RESTRICT,
    quality_id INTEGER,
    revision_json TEXT CHECK (revision_json IS NULL OR (json_valid(revision_json) AND json_type(revision_json)='object')),
    languages_json TEXT CHECK (languages_json IS NULL OR (json_valid(languages_json) AND json_type(languages_json)='array' AND json_array_length(languages_json)<=64)),
    media_info_json TEXT CHECK (media_info_json IS NULL OR (length(CAST(media_info_json AS BLOB))<=65536 AND json_valid(media_info_json) AND json_type(media_info_json)='object')),
    size INTEGER CHECK (size>=0),
    date_added TEXT,
    season_number INTEGER CHECK (season_number>=0),
    original_file_path TEXT,
    release_group TEXT,
    indexer_flags INTEGER CHECK (indexer_flags>=0 AND indexer_flags<=CASE media_type WHEN 'tv' THEN 511 ELSE 4095 END),
    release_type INTEGER CHECK (release_type BETWEEN 0 AND 3),
    CHECK ((media_type='tv' AND episode_file_id IS NOT NULL AND movie_file_id IS NULL AND original_file_path IS NULL) OR
           (media_type='movies' AND movie_file_id IS NOT NULL AND episode_file_id IS NULL AND season_number IS NULL AND release_type IS NULL)),
    FOREIGN KEY(media_type,quality_id) REFERENCES quality_definitions(media_type,quality_id)
);
