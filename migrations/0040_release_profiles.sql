-- Release restrictions apply as a set; they have neither an ordered fallback nor scores.
CREATE TABLE release_profile_domains (
 media_type TEXT PRIMARY KEY NOT NULL CHECK(media_type IN ('tv','movies')),
 revision INTEGER NOT NULL DEFAULT 1 CHECK(typeof(revision)='integer' AND revision BETWEEN 1 AND 9007199254740991),
 locally_edited INTEGER NOT NULL DEFAULT 0 CHECK(locally_edited IN (0,1))
);
INSERT INTO release_profile_domains(media_type)VALUES('tv'),('movies');
CREATE TABLE release_profiles (
 id INTEGER PRIMARY KEY CHECK(id BETWEEN 1 AND 9007199254740991),
 media_type TEXT NOT NULL REFERENCES release_profile_domains(media_type),
 name TEXT CHECK(name IS NULL OR length(CAST(name AS BLOB))<=256),
 enabled INTEGER NOT NULL CHECK(enabled IN (0,1)),
 required_json TEXT NOT NULL CHECK(json_valid(required_json) AND json_type(required_json)='array' AND json_array_length(required_json)<=200 AND length(CAST(required_json AS BLOB))<=2459000),
 ignored_json TEXT NOT NULL CHECK(json_valid(ignored_json) AND json_type(ignored_json)='array' AND json_array_length(ignored_json)<=200 AND length(CAST(ignored_json AS BLOB))<=2459000),
 air_date_restriction INTEGER CHECK(air_date_restriction IS NULL OR air_date_restriction IN (0,1)),
 air_date_grace_period_days INTEGER CHECK(air_date_grace_period_days IS NULL OR (typeof(air_date_grace_period_days)='integer' AND air_date_grace_period_days BETWEEN -2147483648 AND 2147483647)),
 allow_season_pack_without_all_episodes_aired INTEGER CHECK(allow_season_pack_without_all_episodes_aired IS NULL OR allow_season_pack_without_all_episodes_aired IN (0,1)),
 UNIQUE(id,media_type),
 CHECK((media_type='movies' AND air_date_restriction IS NULL AND air_date_grace_period_days IS NULL AND allow_season_pack_without_all_episodes_aired IS NULL) OR (media_type='tv' AND air_date_restriction IS NOT NULL AND air_date_grace_period_days IS NOT NULL AND allow_season_pack_without_all_episodes_aired IS NOT NULL)),
 CHECK(json_array_length(required_json)+json_array_length(ignored_json)>0 OR (media_type='tv' AND (air_date_restriction=1 OR allow_season_pack_without_all_episodes_aired=1)))
);
CREATE TRIGGER release_profile_identity BEFORE UPDATE ON release_profiles WHEN NEW.id IS NOT OLD.id OR NEW.media_type IS NOT OLD.media_type BEGIN SELECT RAISE(ABORT,'release profile identity is immutable'); END;
CREATE TRIGGER release_profile_capacity BEFORE INSERT ON release_profiles WHEN NOT EXISTS(SELECT 1 FROM release_profiles WHERE id=NEW.id) AND (SELECT count(*) FROM release_profiles WHERE media_type=NEW.media_type)>=1024 BEGIN SELECT RAISE(ABORT,'release profile capacity'); END;
CREATE TABLE release_profile_tags (
 profile_id INTEGER NOT NULL,
 media_type TEXT NOT NULL,
 tag_id INTEGER NOT NULL,
 kind TEXT NOT NULL CHECK(kind IN ('include','exclude') AND (kind<>'exclude' OR media_type='tv')),
 PRIMARY KEY(profile_id,tag_id),
 FOREIGN KEY(profile_id,media_type)REFERENCES release_profiles(id,media_type) ON DELETE CASCADE,
 FOREIGN KEY(tag_id,media_type)REFERENCES tags(id,media_type)
);
CREATE TABLE release_profile_indexers (
 profile_id INTEGER NOT NULL,
 media_type TEXT NOT NULL,
 reference_key TEXT NOT NULL,
 provider_id TEXT REFERENCES providers(id),
 source_application TEXT,
 source_fingerprint TEXT,
 source_id INTEGER,
 PRIMARY KEY(profile_id,reference_key),
 FOREIGN KEY(profile_id,media_type)REFERENCES release_profiles(id,media_type) ON DELETE CASCADE,
 CHECK((provider_id IS NOT NULL AND reference_key='provider:'||provider_id AND source_application IS NULL AND source_fingerprint IS NULL AND source_id IS NULL) OR (provider_id IS NULL AND source_application IS NOT NULL AND source_application=CASE media_type WHEN 'tv' THEN 'sonarr' WHEN 'movies' THEN 'radarr' END AND source_fingerprint IS NOT NULL AND length(source_fingerprint)=64 AND source_fingerprint NOT GLOB '*[^0-9a-f]*' AND source_id IS NOT NULL AND typeof(source_id)='integer' AND source_id BETWEEN 1 AND 9007199254740991 AND reference_key='source:'||source_application||':'||source_fingerprint||':'||source_id))
);
CREATE INDEX release_profile_provider_refs ON release_profile_indexers(provider_id);
CREATE INDEX release_profile_tag_refs ON release_profile_tags(tag_id);
CREATE TRIGGER release_profile_tag_identity BEFORE UPDATE ON release_profile_tags BEGIN SELECT RAISE(ABORT,'replace release profile tag explicitly'); END;
CREATE TRIGGER release_profile_indexer_identity BEFORE UPDATE ON release_profile_indexers BEGIN SELECT RAISE(ABORT,'replace release profile indexer explicitly'); END;
CREATE TRIGGER release_profile_tag_capacity BEFORE INSERT ON release_profile_tags WHEN (SELECT count(*) FROM release_profile_tags WHERE profile_id=NEW.profile_id)>=200 BEGIN SELECT RAISE(ABORT,'release profile tag capacity'); END;
CREATE TRIGGER release_profile_indexer_capacity BEFORE INSERT ON release_profile_indexers WHEN (SELECT count(*) FROM release_profile_indexers WHERE profile_id=NEW.profile_id)>=CASE NEW.media_type WHEN 'movies' THEN 1 ELSE 200 END BEGIN SELECT RAISE(ABORT,'release profile indexer capacity'); END;
CREATE TRIGGER release_profile_unresolved_insert BEFORE INSERT ON release_profile_indexers WHEN NEW.provider_id IS NULL AND EXISTS(SELECT 1 FROM release_profiles WHERE id=NEW.profile_id AND enabled=1) BEGIN SELECT RAISE(ABORT,'unresolved release profile must be disabled'); END;
CREATE TRIGGER release_profile_unresolved_enable BEFORE UPDATE OF enabled ON release_profiles WHEN NEW.enabled=1 AND EXISTS(SELECT 1 FROM release_profile_indexers WHERE profile_id=NEW.id AND provider_id IS NULL) BEGIN SELECT RAISE(ABORT,'resolve indexers before enabling'); END;
ALTER TABLE snapshot_imports ADD COLUMN release_profile_version INTEGER NOT NULL DEFAULT 0 CHECK(release_profile_version IN (0,1));
