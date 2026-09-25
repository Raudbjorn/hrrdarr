-- Imported event facts are source attestations, never local file-transfer receipts.
ALTER TABLE snapshot_imports ADD COLUMN history_version INTEGER NOT NULL DEFAULT 0 CHECK(history_version IN (0,1));
CREATE TABLE snapshot_history_events (
 application TEXT NOT NULL CHECK(application IN ('sonarr','radarr')),
 fingerprint TEXT NOT NULL,
 source_id INTEGER NOT NULL CHECK(typeof(source_id)='integer' AND source_id BETWEEN 1 AND 9007199254740991),
 media_type TEXT NOT NULL CHECK(media_type IN ('episode','movie')),
 episode_id INTEGER REFERENCES episodes(id) ON DELETE RESTRICT,
 movie_id INTEGER REFERENCES movies(id) ON DELETE RESTRICT,
 occurred_at TEXT NOT NULL CHECK(length(occurred_at) BETWEEN 19 AND 29),
 event_type TEXT NOT NULL,
 source_event_type INTEGER NOT NULL CHECK(typeof(source_event_type)='integer'),
 source_title TEXT CHECK(source_title IS NULL OR (typeof(source_title)='text' AND length(CAST(source_title AS BLOB))<=1024 AND instr(source_title,char(0))=0)),
 download_id TEXT CHECK(download_id IS NULL OR (typeof(download_id)='text' AND length(CAST(download_id AS BLOB)) BETWEEN 1 AND 256 AND download_id NOT GLOB '*[^A-Za-z0-9._:-]*')),
 quality_id INTEGER CHECK(quality_id IS NULL OR (typeof(quality_id)='integer' AND quality_id BETWEEN 0 AND 9007199254740991)),
 quality_revision_json TEXT CHECK(quality_revision_json IS NULL OR (quality_id IS NOT NULL AND length(CAST(quality_revision_json AS BLOB))<=512 AND json_valid(quality_revision_json) AND json_type(quality_revision_json)='object')),
 languages_json TEXT CHECK(languages_json IS NULL OR (length(CAST(languages_json AS BLOB))<=512 AND json_valid(languages_json) AND json_type(languages_json)='array' AND json_array_length(languages_json)<=64)),
 PRIMARY KEY(application,fingerprint,source_id),
 FOREIGN KEY(application,fingerprint) REFERENCES snapshot_imports(application,fingerprint) ON DELETE RESTRICT,
 CHECK((application='sonarr' AND media_type='episode' AND episode_id IS NOT NULL AND movie_id IS NULL)
    OR (application='radarr' AND media_type='movie' AND episode_id IS NULL AND movie_id IS NOT NULL)),
 CHECK((source_event_type=1 AND event_type='grabbed')
    OR (application='sonarr' AND source_event_type=2 AND event_type='series_folder_imported')
    OR (source_event_type=3 AND event_type='download_folder_imported')
    OR (source_event_type=4 AND event_type='download_failed')
    OR (application='sonarr' AND source_event_type=5 AND event_type='file_deleted')
    OR (application='sonarr' AND source_event_type=6 AND event_type='file_renamed')
    OR (application='sonarr' AND source_event_type=7 AND event_type='download_ignored')
    OR (application='radarr' AND source_event_type=6 AND event_type='file_deleted')
    OR (application='radarr' AND source_event_type=7 AND event_type='movie_folder_imported')
    OR (application='radarr' AND source_event_type=8 AND event_type='file_renamed')
    OR (application='radarr' AND source_event_type=9 AND event_type='download_ignored'))
);
CREATE INDEX snapshot_history_order ON snapshot_history_events(occurred_at DESC,application,fingerprint,source_id DESC);
CREATE INDEX snapshot_history_episode ON snapshot_history_events(episode_id,occurred_at DESC);
CREATE INDEX snapshot_history_movie ON snapshot_history_events(movie_id,occurred_at DESC);
CREATE TRIGGER snapshot_history_immutable BEFORE UPDATE ON snapshot_history_events
BEGIN SELECT RAISE(ABORT,'source history is immutable'); END;
CREATE TRIGGER snapshot_history_retained BEFORE DELETE ON snapshot_history_events
BEGIN SELECT RAISE(ABORT,'source history must be retained'); END;
