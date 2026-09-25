-- Nullable means not supplied by the catalog/snapshot; existing rows are not given invented metadata.
ALTER TABLE episodes ADD COLUMN tvdb_id INTEGER CHECK (tvdb_id > 0);
ALTER TABLE episodes ADD COLUMN air_date TEXT;
ALTER TABLE episodes ADD COLUMN air_date_utc TEXT;
ALTER TABLE episodes ADD COLUMN last_search_time TEXT;
ALTER TABLE episodes ADD COLUMN runtime INTEGER CHECK (runtime >= 0);
ALTER TABLE episodes ADD COLUMN finale_type TEXT;
ALTER TABLE episodes ADD COLUMN overview TEXT;
ALTER TABLE episodes ADD COLUMN absolute_episode_number INTEGER CHECK (absolute_episode_number >= 0);
ALTER TABLE episodes ADD COLUMN scene_absolute_episode_number INTEGER CHECK (scene_absolute_episode_number >= 0);
ALTER TABLE episodes ADD COLUMN scene_episode_number INTEGER CHECK (scene_episode_number >= 0);
ALTER TABLE episodes ADD COLUMN scene_season_number INTEGER CHECK (scene_season_number >= 0);
ALTER TABLE episodes ADD COLUMN unverified_scene_numbering INTEGER CHECK (unverified_scene_numbering IN (0,1));
ALTER TABLE episodes ADD COLUMN images_json TEXT CHECK (images_json IS NULL OR (json_valid(images_json) AND json_type(images_json)='array' AND json_array_length(images_json)<=32));
CREATE INDEX episodes_external_identity ON episodes(tvdb_id) WHERE tvdb_id IS NOT NULL;
-- Existing snapshot mappings may acquire newly supported metadata once on exact replay.
ALTER TABLE snapshot_imports ADD COLUMN episode_metadata_version INTEGER NOT NULL DEFAULT 0 CHECK (episode_metadata_version IN (0,1));
