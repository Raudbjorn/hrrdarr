-- Only the delivered indexer/client configuration foundation. Credentials are an opaque
-- authenticated-encryption envelope; the external key never belongs in this database.
CREATE TABLE providers (
    id TEXT PRIMARY KEY NOT NULL CHECK (length(id)=36 AND substr(id,9,1)='-' AND substr(id,14,1)='-' AND substr(id,19,1)='-' AND substr(id,24,1)='-' AND length(replace(id,'-',''))=32 AND replace(id,'-','') NOT GLOB '*[^0-9a-f]*'),
    implementation TEXT NOT NULL CHECK (implementation IN ('torznab','newznab','qbittorrent')),
    name TEXT NOT NULL CHECK (length(trim(name))>0 AND length(CAST(name AS BLOB))<=128),
    enabled INTEGER NOT NULL CHECK (enabled IN (0,1)),
    priority INTEGER NOT NULL CHECK (typeof(priority)='integer' AND priority BETWEEN 1 AND 100),
    revision INTEGER NOT NULL CHECK (typeof(revision)='integer' AND revision BETWEEN 1 AND 9007199254740991),
    settings_version INTEGER NOT NULL CHECK (settings_version=1),
    endpoint TEXT NOT NULL CHECK (length(trim(endpoint))>0 AND length(CAST(endpoint AS BLOB))<=2048),
    credentials BLOB CHECK (credentials IS NULL OR (typeof(credentials)='blob' AND length(credentials) BETWEEN 29 AND 16384)),
    UNIQUE(id,implementation)
);
CREATE INDEX providers_order ON providers(priority,name,id);
CREATE TRIGGER provider_identity_immutable BEFORE UPDATE OF id,implementation,settings_version ON providers
WHEN NEW.id IS NOT OLD.id OR NEW.implementation IS NOT OLD.implementation OR NEW.settings_version IS NOT OLD.settings_version
BEGIN SELECT RAISE(ABORT,'provider identity is immutable'); END;
CREATE TRIGGER provider_revision_step BEFORE UPDATE ON providers
WHEN NEW.revision IS NOT OLD.revision+1
BEGIN SELECT RAISE(ABORT,'provider update requires the next revision'); END;

CREATE TABLE provider_scopes (
    provider_id TEXT NOT NULL,
    implementation TEXT NOT NULL,
    media_type TEXT NOT NULL CHECK (media_type IN ('tv','movies')),
    categories TEXT CHECK (categories IS NULL OR (length(CAST(categories AS BLOB))<=4096 AND json_valid(categories) AND json_type(categories)='array' AND json_array_length(categories)<=64)),
    anime_categories TEXT CHECK (anime_categories IS NULL OR (length(CAST(anime_categories AS BLOB))<=4096 AND json_valid(anime_categories) AND json_type(anime_categories)='array' AND json_array_length(anime_categories)<=64)),
    category TEXT CHECK (category IS NULL OR (length(trim(category))>0 AND length(CAST(category AS BLOB))<=64)),
    imported_category TEXT CHECK (imported_category IS NULL OR (length(trim(imported_category))>0 AND length(CAST(imported_category AS BLOB))<=64)),
    recent_priority INTEGER CHECK (recent_priority IS NULL OR (typeof(recent_priority)='integer' AND recent_priority IN (0,1))),
    older_priority INTEGER CHECK (older_priority IS NULL OR (typeof(older_priority)='integer' AND older_priority IN (0,1))),
    PRIMARY KEY(provider_id,media_type),
    FOREIGN KEY(provider_id,implementation) REFERENCES providers(id,implementation) ON DELETE CASCADE,
    CHECK ((implementation IN ('torznab','newznab') AND categories IS NOT NULL AND anime_categories IS NOT NULL
        AND category IS NULL AND imported_category IS NULL AND recent_priority IS NULL AND older_priority IS NULL
        AND ((media_type='movies' AND json_array_length(categories)>0 AND json_array_length(anime_categories)=0)
          OR (media_type='tv' AND json_array_length(categories)+json_array_length(anime_categories)>0)))
      OR (implementation='qbittorrent' AND categories IS NULL AND anime_categories IS NULL AND category IS NOT NULL AND recent_priority IS NOT NULL AND older_priority IS NOT NULL))
);
-- A shared client must not classify both media domains into the same active category.
CREATE UNIQUE INDEX provider_client_categories ON provider_scopes(provider_id,category) WHERE implementation='qbittorrent';
CREATE TRIGGER provider_categories_insert BEFORE INSERT ON provider_scopes
WHEN EXISTS(SELECT 1 FROM json_each(NEW.categories) WHERE type!='integer' OR value<=0 OR value>2147483647)
 OR EXISTS(SELECT 1 FROM json_each(NEW.anime_categories) WHERE type!='integer' OR value<=0 OR value>2147483647)
 OR (SELECT count(*) FROM json_each(NEW.categories))!=(SELECT count(DISTINCT value) FROM json_each(NEW.categories))
 OR (SELECT count(*) FROM json_each(NEW.anime_categories))!=(SELECT count(DISTINCT value) FROM json_each(NEW.anime_categories))
BEGIN SELECT RAISE(ABORT,'invalid provider category IDs'); END;
CREATE TRIGGER provider_categories_update BEFORE UPDATE OF categories,anime_categories ON provider_scopes
WHEN EXISTS(SELECT 1 FROM json_each(NEW.categories) WHERE type!='integer' OR value<=0 OR value>2147483647)
 OR EXISTS(SELECT 1 FROM json_each(NEW.anime_categories) WHERE type!='integer' OR value<=0 OR value>2147483647)
 OR (SELECT count(*) FROM json_each(NEW.categories))!=(SELECT count(DISTINCT value) FROM json_each(NEW.categories))
 OR (SELECT count(*) FROM json_each(NEW.anime_categories))!=(SELECT count(DISTINCT value) FROM json_each(NEW.anime_categories))
BEGIN SELECT RAISE(ABORT,'invalid provider category IDs'); END;
