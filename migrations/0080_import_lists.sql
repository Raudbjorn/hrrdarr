-- Import lists (Sonarr "Import Lists" / Radarr "Lists"): a registry of external list sources per media
-- domain, a per-domain exclusion list, a bounded membership snapshot per list, a durable resumable
-- sync-run journal and add-provenance rows. Implementation names are validated in Rust, so a new
-- provider needs no migration. Secrets (API tokens) are AEAD-sealed in `credentials`; only their
-- names are stored in clear.
CREATE TABLE import_lists (
 id TEXT PRIMARY KEY NOT NULL CHECK(length(id)=36 AND substr(id,9,1)='-' AND substr(id,14,1)='-' AND substr(id,19,1)='-' AND substr(id,24,1)='-' AND length(replace(id,'-',''))=32 AND replace(id,'-','') NOT GLOB '*[^0-9a-f]*'),
 media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
 name TEXT NOT NULL CHECK(typeof(name)='text' AND length(CAST(name AS BLOB)) BETWEEN 1 AND 128 AND name=trim(name)),
 implementation TEXT NOT NULL CHECK(typeof(implementation)='text' AND length(implementation) BETWEEN 1 AND 32 AND implementation NOT GLOB '*[^a-z0-9_]*'),
 enabled INTEGER NOT NULL DEFAULT 1 CHECK(typeof(enabled)='integer' AND enabled IN (0,1)),
 -- Off: the list is fetched and its membership recorded, but nothing is added to the library.
 enable_automatic_add INTEGER NOT NULL DEFAULT 1 CHECK(typeof(enable_automatic_add)='integer' AND enable_automatic_add IN (0,1)),
 search_on_add INTEGER NOT NULL DEFAULT 0 CHECK(typeof(search_on_add)='integer' AND search_on_add IN (0,1)),
 -- TV: which seasons start monitored. Movies: only all (monitored) or none.
 monitor TEXT NOT NULL DEFAULT 'all' CHECK(monitor IN ('all','none','first_season','last_season') AND (media_type='tv' OR monitor IN ('all','none'))),
 -- Defaults applied to items this list adds. NULL profile or root disables adding until configured.
 quality_profile_id INTEGER CHECK(quality_profile_id IS NULL OR typeof(quality_profile_id)='integer'),
 root_folder_id INTEGER CHECK(root_folder_id IS NULL OR typeof(root_folder_id)='integer'),
 minimum_availability TEXT CHECK((media_type='movies' AND minimum_availability IN ('tba','announced','in_cinemas','released')) OR (media_type='tv' AND minimum_availability IS NULL)),
 series_type TEXT CHECK((media_type='tv' AND series_type IN ('standard','daily','anime')) OR (media_type='movies' AND series_type IS NULL)),
 season_folder INTEGER CHECK((media_type='tv' AND season_folder IN (0,1)) OR (media_type='movies' AND season_folder IS NULL)),
 interval_minutes INTEGER NOT NULL DEFAULT 1440 CHECK(typeof(interval_minutes)='integer' AND interval_minutes BETWEEN 5 AND 43200),
 revision INTEGER NOT NULL DEFAULT 1 CHECK(typeof(revision)='integer' AND revision BETWEEN 1 AND 9007199254740991),
 settings_json TEXT NOT NULL CHECK(typeof(settings_json)='text' AND length(CAST(settings_json AS BLOB))<=16384 AND json_valid(settings_json) AND json_type(settings_json)='object'),
 secret_names TEXT NOT NULL DEFAULT '[]' CHECK(typeof(secret_names)='text' AND length(secret_names)<=2048 AND json_valid(secret_names) AND json_type(secret_names)='array'),
 credentials BLOB CHECK(credentials IS NULL OR (typeof(credentials)='blob' AND length(credentials) BETWEEN 29 AND 16384)),
 -- Last completed sync attempt and last success (a failed fetch keeps the previous snapshot).
 last_sync_at INTEGER CHECK(last_sync_at IS NULL OR typeof(last_sync_at)='integer'),
 last_success_at INTEGER CHECK(last_success_at IS NULL OR typeof(last_success_at)='integer'),
 last_error TEXT CHECK(last_error IS NULL OR (length(last_error) BETWEEN 1 AND 64 AND last_error NOT GLOB '*[^a-z0-9_]*')),
 created_at INTEGER NOT NULL CHECK(typeof(created_at)='integer' AND created_at BETWEEN 0 AND 9007199254740991),
 updated_at INTEGER NOT NULL CHECK(typeof(updated_at)='integer' AND updated_at BETWEEN 0 AND 9007199254740991),
 UNIQUE(id,media_type)
);
CREATE UNIQUE INDEX import_lists_name ON import_lists(media_type,lower(name));
CREATE TRIGGER import_lists_capacity BEFORE INSERT ON import_lists
WHEN (SELECT count(*) FROM import_lists)>=64 BEGIN SELECT RAISE(ABORT,'import list capacity reached'); END;
CREATE TRIGGER import_lists_identity BEFORE UPDATE ON import_lists
WHEN NEW.id IS NOT OLD.id OR NEW.media_type IS NOT OLD.media_type OR NEW.implementation IS NOT OLD.implementation OR NEW.created_at IS NOT OLD.created_at
BEGIN SELECT RAISE(ABORT,'import list identity is immutable'); END;
-- Sync bookkeeping (last_*) does not advance the user-visible revision; configuration edits do.
CREATE TRIGGER import_lists_revision BEFORE UPDATE ON import_lists
WHEN NEW.revision!=OLD.revision+1 AND NEW.revision!=OLD.revision
BEGIN SELECT RAISE(ABORT,'import list revisions advance by one'); END;
-- A deleted quality profile or root folder disables adding instead of dangling (same policy as
-- collection defaults): the list keeps syncing membership but cannot add until reconfigured.
CREATE TRIGGER import_lists_profile_deleted BEFORE DELETE ON quality_profiles
BEGIN UPDATE import_lists SET quality_profile_id=NULL,enable_automatic_add=0 WHERE quality_profile_id=OLD.id AND media_type=OLD.media_type; END;
CREATE TRIGGER import_lists_root_deleted BEFORE DELETE ON root_folders
BEGIN UPDATE import_lists SET root_folder_id=NULL,enable_automatic_add=0 WHERE root_folder_id=OLD.id AND media_type=OLD.media_type; END;

-- Tags applied to every item the list adds. Domain-qualified on both sides; a deleted tag simply
-- leaves the default set.
CREATE TABLE import_list_tags (
 list_id TEXT NOT NULL,
 media_type TEXT NOT NULL,
 tag_id INTEGER NOT NULL,
 PRIMARY KEY(list_id,tag_id),
 FOREIGN KEY(list_id,media_type) REFERENCES import_lists(id,media_type) ON DELETE CASCADE,
 FOREIGN KEY(tag_id,media_type) REFERENCES tags(id,media_type) ON DELETE CASCADE
);
CREATE INDEX import_list_tags_tag ON import_list_tags(tag_id);
CREATE TRIGGER import_list_tag_capacity BEFORE INSERT ON import_list_tags
WHEN (SELECT count(*) FROM import_list_tags WHERE list_id=NEW.list_id)>=200
BEGIN SELECT RAISE(ABORT,'import list tag capacity reached'); END;

-- Items that must never be added by any list of the domain. `external_id` is the TVDB id for TV and
-- the TMDb id for movies; the same number in the other domain is a different row.
CREATE TABLE import_list_exclusions (
 id INTEGER PRIMARY KEY AUTOINCREMENT CHECK(typeof(id)='integer' AND id BETWEEN 1 AND 9007199254740991),
 media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
 external_id INTEGER NOT NULL CHECK(typeof(external_id)='integer' AND external_id BETWEEN 1 AND 9007199254740991),
 title TEXT NOT NULL CHECK(typeof(title)='text' AND length(CAST(title AS BLOB)) BETWEEN 1 AND 1024),
 year INTEGER CHECK(year IS NULL OR (typeof(year)='integer' AND year BETWEEN 1 AND 9999)),
 created_at INTEGER NOT NULL CHECK(typeof(created_at)='integer' AND created_at BETWEEN 0 AND 9007199254740991),
 UNIQUE(media_type,external_id)
);
CREATE TRIGGER import_list_exclusions_capacity BEFORE INSERT ON import_list_exclusions
WHEN (SELECT count(*) FROM import_list_exclusions)>=20000 BEGIN SELECT RAISE(ABORT,'import list exclusion capacity reached'); END;

-- Durable sync journal. One active (queued/running) run per list; a run that stops mid-way keeps
-- its fetched snapshot and per-item states, so the next claim resumes instead of refetching.
CREATE TABLE import_list_runs (
 id TEXT PRIMARY KEY NOT NULL CHECK(length(id)=36),
 list_id TEXT NOT NULL REFERENCES import_lists(id) ON DELETE CASCADE,
 media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
 origin TEXT NOT NULL CHECK(origin IN ('manual','scheduled')),
 status TEXT NOT NULL CHECK(status IN ('queued','running','succeeded','failed')),
 -- Snapshot fetched and recorded; later claims skip the network fetch.
 fetched_at INTEGER CHECK(fetched_at IS NULL OR typeof(fetched_at)='integer'),
 attempts INTEGER NOT NULL DEFAULT 0 CHECK(typeof(attempts)='integer' AND attempts BETWEEN 0 AND 8),
 next_attempt_at INTEGER NOT NULL CHECK(typeof(next_attempt_at)='integer'),
 lease_until INTEGER NOT NULL DEFAULT 0 CHECK(typeof(lease_until)='integer'),
 items_fetched INTEGER NOT NULL DEFAULT 0 CHECK(items_fetched BETWEEN 0 AND 100000),
 added INTEGER NOT NULL DEFAULT 0 CHECK(added BETWEEN 0 AND 100000),
 existing INTEGER NOT NULL DEFAULT 0 CHECK(existing BETWEEN 0 AND 100000),
 excluded INTEGER NOT NULL DEFAULT 0 CHECK(excluded BETWEEN 0 AND 100000),
 unresolved INTEGER NOT NULL DEFAULT 0 CHECK(unresolved BETWEEN 0 AND 100000),
 failed INTEGER NOT NULL DEFAULT 0 CHECK(failed BETWEEN 0 AND 100000),
 error_code TEXT CHECK(error_code IS NULL OR (length(error_code) BETWEEN 1 AND 64 AND error_code NOT GLOB '*[^a-z0-9_]*')),
 created_at INTEGER NOT NULL CHECK(typeof(created_at)='integer'),
 started_at INTEGER CHECK(started_at IS NULL OR typeof(started_at)='integer'),
 completed_at INTEGER CHECK(completed_at IS NULL OR typeof(completed_at)='integer'),
 CHECK((status IN ('succeeded','failed'))=(completed_at IS NOT NULL))
);
CREATE UNIQUE INDEX import_list_runs_active ON import_list_runs(list_id) WHERE status IN ('queued','running');
CREATE INDEX import_list_runs_due ON import_list_runs(status,next_attempt_at);
CREATE INDEX import_list_runs_list ON import_list_runs(list_id,created_at);
CREATE TRIGGER import_list_runs_retention AFTER UPDATE OF status ON import_list_runs
WHEN NEW.status IN ('succeeded','failed')
BEGIN
 DELETE FROM import_list_runs WHERE list_id=NEW.list_id AND status IN ('succeeded','failed')
  AND id NOT IN (SELECT id FROM import_list_runs WHERE list_id=NEW.list_id AND status IN ('succeeded','failed') ORDER BY created_at DESC,rowid DESC LIMIT 32);
END;

-- Bounded membership snapshot of the last complete fetch. `key` is the item's canonical identity as
-- the list supplied it (tvdb:N, tmdb:N, imdb:ttN, or title:<normalised>:<year>).
CREATE TABLE import_list_items (
 list_id TEXT NOT NULL REFERENCES import_lists(id) ON DELETE CASCADE,
 key TEXT NOT NULL CHECK(length(key) BETWEEN 3 AND 300),
 seq INTEGER NOT NULL CHECK(seq BETWEEN 0 AND 100000),
 tvdb_id INTEGER CHECK(tvdb_id IS NULL OR tvdb_id BETWEEN 1 AND 9007199254740991),
 tmdb_id INTEGER CHECK(tmdb_id IS NULL OR tmdb_id BETWEEN 1 AND 9007199254740991),
 imdb_id TEXT CHECK(imdb_id IS NULL OR (imdb_id GLOB 'tt[0-9]*' AND length(imdb_id) BETWEEN 3 AND 32)),
 title TEXT NOT NULL CHECK(length(CAST(title AS BLOB)) BETWEEN 1 AND 1024),
 year INTEGER CHECK(year IS NULL OR year BETWEEN 1 AND 9999),
 state TEXT NOT NULL DEFAULT 'pending' CHECK(state IN ('pending','added','existing','excluded','unresolved','failed')),
 error_code TEXT CHECK(error_code IS NULL OR (length(error_code) BETWEEN 1 AND 64 AND error_code NOT GLOB '*[^a-z0-9_]*')),
 library_id INTEGER CHECK(library_id IS NULL OR library_id>=1),
 updated_at INTEGER NOT NULL CHECK(typeof(updated_at)='integer'),
 PRIMARY KEY(list_id,key)
);
CREATE INDEX import_list_items_pending ON import_list_items(list_id,state,seq);
CREATE TRIGGER import_list_items_capacity BEFORE INSERT ON import_list_items
WHEN (SELECT count(*) FROM import_list_items WHERE list_id=NEW.list_id)>=5000
BEGIN SELECT RAISE(ABORT,'import list item capacity reached'); END;

-- Provenance: which list and run added a library record. The list link is nulled (name retained)
-- if the list is deleted; the record disappears with its series or movie.
CREATE TABLE import_list_additions (
 id INTEGER PRIMARY KEY AUTOINCREMENT,
 media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
 series_id INTEGER UNIQUE REFERENCES series(id) ON DELETE CASCADE,
 movie_id INTEGER UNIQUE REFERENCES movies(id) ON DELETE CASCADE,
 list_id TEXT REFERENCES import_lists(id) ON DELETE SET NULL,
 list_name TEXT NOT NULL CHECK(length(CAST(list_name AS BLOB)) BETWEEN 1 AND 128),
 run_id TEXT CHECK(run_id IS NULL OR length(run_id)=36),
 external_id INTEGER NOT NULL CHECK(external_id>=1),
 search_requested INTEGER NOT NULL DEFAULT 0 CHECK(search_requested IN (0,1)),
 added_at INTEGER NOT NULL CHECK(typeof(added_at)='integer'),
 CHECK((media_type='tv' AND series_id IS NOT NULL AND movie_id IS NULL) OR (media_type='movies' AND movie_id IS NOT NULL AND series_id IS NULL))
);
CREATE INDEX import_list_additions_list ON import_list_additions(list_id);
