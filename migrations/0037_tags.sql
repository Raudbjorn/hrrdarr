-- Domain-scoped catalogs and actual library assignments; provenance mappings survive deletion.
CREATE TABLE tags (
 id INTEGER PRIMARY KEY CHECK(id BETWEEN 1 AND 9007199254740991),
 media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
 label TEXT NOT NULL CHECK(length(label) BETWEEN 1 AND 128 AND label NOT GLOB '*[^a-z0-9-]*'),
 UNIQUE(media_type,label), UNIQUE(id,media_type)
);
CREATE TABLE series_tags (
 series_id INTEGER NOT NULL REFERENCES series(id) ON DELETE CASCADE,
 tag_id INTEGER NOT NULL,
 media_type TEXT NOT NULL DEFAULT 'tv' CHECK(media_type='tv'),
 PRIMARY KEY(series_id,tag_id), FOREIGN KEY(tag_id,media_type) REFERENCES tags(id,media_type)
);
CREATE INDEX series_tags_tag ON series_tags(tag_id,series_id);
CREATE TABLE movie_tags (
 movie_id INTEGER NOT NULL REFERENCES movies(id) ON DELETE CASCADE,
 tag_id INTEGER NOT NULL,
 media_type TEXT NOT NULL DEFAULT 'movies' CHECK(media_type='movies'),
 PRIMARY KEY(movie_id,tag_id), FOREIGN KEY(tag_id,media_type) REFERENCES tags(id,media_type)
);
CREATE INDEX movie_tags_tag ON movie_tags(tag_id,movie_id);
-- A native empty replacement is still a local edit; absence alone cannot authorize backfill.
CREATE TABLE library_tag_edits (
 series_id INTEGER UNIQUE REFERENCES series(id) ON DELETE CASCADE,
 movie_id INTEGER UNIQUE REFERENCES movies(id) ON DELETE CASCADE,
 CHECK((series_id IS NOT NULL)+(movie_id IS NOT NULL)=1)
);
ALTER TABLE snapshot_imports ADD COLUMN tag_version INTEGER NOT NULL DEFAULT 0 CHECK(tag_version IN (0,1));
