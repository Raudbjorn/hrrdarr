-- Movie collections (api.036/ui.037/job.012). Catalog identity already exists (0002); this adds the
-- local add-missing policy, tags and the persisted RefreshCollections command. Membership rows
-- (movie_collection_members) reference catalog metadata, never library membership.
ALTER TABLE movie_collections ADD COLUMN overview TEXT CHECK(overview IS NULL OR (typeof(overview)='text' AND length(CAST(overview AS BLOB))<=65536 AND instr(overview,char(0))=0));
ALTER TABLE movie_collections ADD COLUMN monitored INTEGER NOT NULL DEFAULT 0 CHECK(monitored IN (0,1));
ALTER TABLE movie_collections ADD COLUMN search_on_add INTEGER NOT NULL DEFAULT 0 CHECK(search_on_add IN (0,1));
ALTER TABLE movie_collections ADD COLUMN quality_profile_id INTEGER CHECK(quality_profile_id IS NULL OR (typeof(quality_profile_id)='integer' AND quality_profile_id>0));
ALTER TABLE movie_collections ADD COLUMN root_folder_id INTEGER REFERENCES root_folders(id) ON DELETE SET NULL;
ALTER TABLE movie_collections ADD COLUMN minimum_availability TEXT CHECK(minimum_availability IS NULL OR minimum_availability IN ('tba','announced','in_cinemas','released'));
ALTER TABLE movie_collections ADD COLUMN last_info_sync TEXT CHECK(last_info_sync IS NULL OR (typeof(last_info_sync)='text' AND length(last_info_sync)=20));
CREATE TRIGGER movie_collections_profile_insert BEFORE INSERT ON movie_collections
WHEN NEW.quality_profile_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM quality_profiles WHERE id=NEW.quality_profile_id AND media_type='movies')
BEGIN SELECT RAISE(ABORT,'collection_profile_domain'); END;
CREATE TRIGGER movie_collections_profile_update BEFORE UPDATE OF quality_profile_id ON movie_collections
WHEN NEW.quality_profile_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM quality_profiles WHERE id=NEW.quality_profile_id AND media_type='movies')
BEGIN SELECT RAISE(ABORT,'collection_profile_domain'); END;
CREATE TRIGGER movie_collections_root_update BEFORE UPDATE OF root_folder_id ON movie_collections
WHEN NEW.root_folder_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM root_folders WHERE id=NEW.root_folder_id AND media_type='movies')
BEGIN SELECT RAISE(ABORT,'collection_root_domain'); END;
CREATE TRIGGER movie_collections_monitor_policy BEFORE UPDATE OF monitored ON movie_collections
WHEN NEW.monitored=1 AND (NEW.quality_profile_id IS NULL OR NEW.root_folder_id IS NULL OR NEW.minimum_availability IS NULL)
BEGIN SELECT RAISE(ABORT,'collection_policy_incomplete'); END;
CREATE TABLE movie_collection_tags (
 collection_id INTEGER NOT NULL REFERENCES movie_collections(id) ON DELETE CASCADE,
 tag_id INTEGER NOT NULL,
 media_type TEXT NOT NULL DEFAULT 'movies' CHECK(media_type='movies'),
 PRIMARY KEY(collection_id,tag_id),
 FOREIGN KEY(tag_id,media_type) REFERENCES tags(id,media_type) ON DELETE RESTRICT
);
CREATE INDEX movie_collection_tags_tag ON movie_collection_tags(tag_id,collection_id);
-- RefreshCollections: at most one active command; independent of the shared command pool.
CREATE TABLE collection_refresh_commands (
 id TEXT PRIMARY KEY NOT NULL CHECK(typeof(id)='text' AND length(id)=36),
 name TEXT NOT NULL CHECK(name='refresh_collections'),
 origin TEXT NOT NULL CHECK(origin IN ('manual','automatic')),
 status TEXT NOT NULL DEFAULT 'queued' CHECK(status IN ('queued','running','retry_wait','succeeded','failed','cancelled')),
 attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts>=0),
 -- Highest collection id already processed; a run may span several worker slices.
 cursor INTEGER NOT NULL DEFAULT 0 CHECK(cursor>=0),
 next_attempt_at INTEGER NOT NULL CHECK(next_attempt_at>=0),
 created_at INTEGER NOT NULL CHECK(created_at>=0),
 started_at INTEGER,
 completed_at INTEGER,
 error_code TEXT CHECK(error_code IS NULL OR error_code IN ('interrupted','storage_error','metadata_unavailable')),
 collections_checked INTEGER NOT NULL DEFAULT 0 CHECK(collections_checked>=0),
 collections_failed INTEGER NOT NULL DEFAULT 0 CHECK(collections_failed>=0),
 movies_added INTEGER NOT NULL DEFAULT 0 CHECK(movies_added>=0),
 searches_pending INTEGER NOT NULL DEFAULT 0 CHECK(searches_pending>=0)
);
CREATE UNIQUE INDEX collection_refresh_active ON collection_refresh_commands((1)) WHERE status IN ('queued','running','retry_wait');
CREATE INDEX collection_refresh_history ON collection_refresh_commands(created_at,id);
