-- Source-backed management only; no release decision or failed-download producer implied.
ALTER TABLE snapshot_imports ADD COLUMN blocklist_version INTEGER NOT NULL DEFAULT 0 CHECK(blocklist_version IN (0,1));
CREATE TABLE snapshot_blocklist (
 application TEXT NOT NULL CHECK(application IN ('sonarr','radarr')),
 fingerprint TEXT NOT NULL,
 source_id INTEGER NOT NULL CHECK(typeof(source_id)='integer' AND source_id BETWEEN 1 AND 9007199254740991),
 facts_digest TEXT NOT NULL CHECK(length(facts_digest)=64 AND facts_digest NOT GLOB '*[^0-9a-f]*'),
 removed_at TEXT,
 PRIMARY KEY(application,fingerprint,source_id),
 FOREIGN KEY(application,fingerprint) REFERENCES snapshot_imports(application,fingerprint) ON DELETE RESTRICT
);
CREATE TABLE blocklist_entries (
 application TEXT NOT NULL,
 fingerprint TEXT NOT NULL,
 source_id INTEGER NOT NULL,
 media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
 series_id INTEGER REFERENCES series(id) ON DELETE CASCADE,
 movie_id INTEGER REFERENCES movies(id) ON DELETE CASCADE,
 occurred_at TEXT NOT NULL CHECK(length(occurred_at) BETWEEN 19 AND 29),
 published_at TEXT CHECK(published_at IS NULL OR length(published_at) BETWEEN 19 AND 29),
 source_title TEXT NOT NULL CHECK(typeof(source_title)='text' AND length(CAST(source_title AS BLOB)) BETWEEN 1 AND 1024 AND instr(source_title,char(0))=0),
 protocol INTEGER CHECK(protocol IS NULL OR (typeof(protocol)='integer' AND protocol BETWEEN 0 AND 2)),
 size INTEGER CHECK(size IS NULL OR (typeof(size)='integer' AND size BETWEEN 0 AND 9007199254740991)),
 quality_id INTEGER,
 quality_revision_json TEXT CHECK(quality_revision_json IS NULL OR (quality_id IS NOT NULL AND length(CAST(quality_revision_json AS BLOB))<=512 AND json_valid(quality_revision_json) AND json_type(quality_revision_json)='object')),
 languages_json TEXT CHECK(languages_json IS NULL OR (length(CAST(languages_json AS BLOB))<=512 AND json_valid(languages_json) AND json_type(languages_json)='array' AND json_array_length(languages_json)<=64)),
 PRIMARY KEY(application,fingerprint,source_id),
 UNIQUE(application,fingerprint,source_id,series_id),
 FOREIGN KEY(application,fingerprint,source_id) REFERENCES snapshot_blocklist(application,fingerprint,source_id) ON DELETE RESTRICT,
 FOREIGN KEY(media_type,quality_id) REFERENCES quality_definitions(media_type,quality_id),
 CHECK((application='sonarr' AND media_type='tv' AND series_id IS NOT NULL AND movie_id IS NULL) OR (application='radarr' AND media_type='movies' AND series_id IS NULL AND movie_id IS NOT NULL))
);
-- Existing episode IDs are globally unique; this composite index supports ownership FK checks.
CREATE UNIQUE INDEX episodes_identity_series ON episodes(id,series_id);
CREATE TABLE blocklist_episodes (
 application TEXT NOT NULL,
 fingerprint TEXT NOT NULL,
 source_id INTEGER NOT NULL,
 series_id INTEGER NOT NULL,
 episode_id INTEGER NOT NULL,
 PRIMARY KEY(application,fingerprint,source_id,episode_id),
 FOREIGN KEY(application,fingerprint,source_id,series_id) REFERENCES blocklist_entries(application,fingerprint,source_id,series_id) ON DELETE CASCADE,
 FOREIGN KEY(episode_id,series_id) REFERENCES episodes(id,series_id) ON DELETE RESTRICT
);
CREATE INDEX blocklist_order ON blocklist_entries(occurred_at DESC,application,fingerprint,source_id DESC);
CREATE INDEX blocklist_series ON blocklist_entries(series_id,occurred_at DESC);
CREATE INDEX blocklist_movie ON blocklist_entries(movie_id,occurred_at DESC);
CREATE TRIGGER blocklist_immutable BEFORE UPDATE ON blocklist_entries BEGIN SELECT RAISE(ABORT,'source blocklist facts are immutable'); END;
CREATE TRIGGER blocklist_episode_immutable BEFORE UPDATE ON blocklist_episodes BEGIN SELECT RAISE(ABORT,'source blocklist targets are immutable'); END;
CREATE TRIGGER blocklist_no_resurrection BEFORE INSERT ON blocklist_entries
WHEN EXISTS(SELECT 1 FROM snapshot_blocklist p WHERE p.application=NEW.application AND p.fingerprint=NEW.fingerprint AND p.source_id=NEW.source_id AND p.removed_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT,'removed blocklist provenance cannot reactivate'); END;
CREATE TRIGGER blocklist_removed BEFORE DELETE ON blocklist_entries
BEGIN UPDATE snapshot_blocklist SET removed_at=COALESCE(removed_at,CURRENT_TIMESTAMP) WHERE application=OLD.application AND fingerprint=OLD.fingerprint AND source_id=OLD.source_id; END;
CREATE TRIGGER blocklist_provenance_immutable BEFORE UPDATE ON snapshot_blocklist
WHEN NEW.application IS NOT OLD.application OR NEW.fingerprint IS NOT OLD.fingerprint OR NEW.source_id IS NOT OLD.source_id OR NEW.facts_digest IS NOT OLD.facts_digest OR OLD.removed_at IS NOT NULL OR NEW.removed_at IS NULL
BEGIN SELECT RAISE(ABORT,'blocklist provenance is immutable'); END;
CREATE TRIGGER blocklist_provenance_retained BEFORE DELETE ON snapshot_blocklist BEGIN SELECT RAISE(ABORT,'blocklist provenance must be retained'); END;
CREATE TRIGGER blocklist_episode_retained BEFORE DELETE ON blocklist_episodes
WHEN EXISTS(SELECT 1 FROM blocklist_entries e WHERE e.application=OLD.application AND e.fingerprint=OLD.fingerprint AND e.source_id=OLD.source_id)
BEGIN SELECT RAISE(ABORT,'remove whole blocklist entry, not individual episodes'); END;
CREATE INDEX blocklist_title_order ON blocklist_entries(source_title,application,fingerprint,source_id DESC);
