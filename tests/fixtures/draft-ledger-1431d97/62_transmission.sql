-- Transmission: a second torrent download client. One registry row (kind/protocol drive every
-- protocol-aware trigger and selection query) plus replacement scope shape/option triggers, as
-- anticipated by 0059. provider_scopes is not rebuilt: a domain's placement is either its
-- category (a subfolder of the client download directory that doubles as a label) or an
-- absolute remote directory stored in provider_scope_directories.
INSERT INTO provider_implementations(implementation,kind,protocol) VALUES ('transmission','download_client','torrent');

DROP TRIGGER provider_scope_shape_insert;
CREATE TRIGGER provider_scope_shape_insert BEFORE INSERT ON provider_scopes
WHEN NOT ((NEW.implementation IN ('torznab','newznab') AND NEW.categories IS NOT NULL AND NEW.anime_categories IS NOT NULL
    AND NEW.category IS NULL AND NEW.imported_category IS NULL AND NEW.recent_priority IS NULL AND NEW.older_priority IS NULL
    AND ((NEW.media_type='movies' AND json_array_length(NEW.categories)>0 AND json_array_length(NEW.anime_categories)=0)
      OR (NEW.media_type='tv' AND json_array_length(NEW.categories)+json_array_length(NEW.anime_categories)>0)))
  OR (NEW.implementation='qbittorrent' AND NEW.categories IS NULL AND NEW.anime_categories IS NULL AND NEW.category IS NOT NULL
    AND NEW.recent_priority IN (0,1) AND NEW.older_priority IN (0,1))
  OR (NEW.implementation='sabnzbd' AND NEW.categories IS NULL AND NEW.anime_categories IS NULL AND NEW.category IS NOT NULL
    AND NEW.imported_category IS NULL AND NEW.download_client_id IS NULL
    AND NEW.recent_priority IN (-100,-2,-1,0,1,2) AND NEW.older_priority IN (-100,-2,-1,0,1,2))
  OR (NEW.implementation='transmission' AND NEW.categories IS NULL AND NEW.anime_categories IS NULL
    AND NEW.imported_category IS NULL AND NEW.download_client_id IS NULL
    AND typeof(NEW.recent_priority)='integer' AND NEW.recent_priority IN (0,1)
    AND typeof(NEW.older_priority)='integer' AND NEW.older_priority IN (0,1)
    AND (NEW.category IS NULL OR (typeof(NEW.category)='text' AND length(NEW.category) BETWEEN 1 AND 64
      AND NEW.category NOT GLOB '*[^A-Za-z0-9._-]*' AND NEW.category NOT IN ('.','..')))))
BEGIN SELECT RAISE(ABORT,'invalid provider scope shape'); END;
DROP TRIGGER provider_scope_shape_update;
CREATE TRIGGER provider_scope_shape_update BEFORE UPDATE ON provider_scopes
WHEN NOT ((NEW.implementation IN ('torznab','newznab') AND NEW.categories IS NOT NULL AND NEW.anime_categories IS NOT NULL
    AND NEW.category IS NULL AND NEW.imported_category IS NULL AND NEW.recent_priority IS NULL AND NEW.older_priority IS NULL
    AND ((NEW.media_type='movies' AND json_array_length(NEW.categories)>0 AND json_array_length(NEW.anime_categories)=0)
      OR (NEW.media_type='tv' AND json_array_length(NEW.categories)+json_array_length(NEW.anime_categories)>0)))
  OR (NEW.implementation='qbittorrent' AND NEW.categories IS NULL AND NEW.anime_categories IS NULL AND NEW.category IS NOT NULL
    AND NEW.recent_priority IN (0,1) AND NEW.older_priority IN (0,1))
  OR (NEW.implementation='sabnzbd' AND NEW.categories IS NULL AND NEW.anime_categories IS NULL AND NEW.category IS NOT NULL
    AND NEW.imported_category IS NULL AND NEW.download_client_id IS NULL
    AND NEW.recent_priority IN (-100,-2,-1,0,1,2) AND NEW.older_priority IN (-100,-2,-1,0,1,2))
  OR (NEW.implementation='transmission' AND NEW.categories IS NULL AND NEW.anime_categories IS NULL
    AND NEW.imported_category IS NULL AND NEW.download_client_id IS NULL
    AND typeof(NEW.recent_priority)='integer' AND NEW.recent_priority IN (0,1)
    AND typeof(NEW.older_priority)='integer' AND NEW.older_priority IN (0,1)
    AND (NEW.category IS NULL OR (typeof(NEW.category)='text' AND length(NEW.category) BETWEEN 1 AND 64
      AND NEW.category NOT GLOB '*[^A-Za-z0-9._-]*' AND NEW.category NOT IN ('.','..')))))
BEGIN SELECT RAISE(ABORT,'invalid provider scope shape'); END;

DROP TRIGGER provider_scope_options_insert;
CREATE TRIGGER provider_scope_options_insert BEFORE INSERT ON provider_scopes
WHEN NOT ((NEW.implementation IN ('torznab','newznab') AND NEW.media_type='tv'
    AND typeof(NEW.anime_standard_format_search)='integer' AND NEW.anime_standard_format_search IN (0,1) AND NEW.remove_year IS NULL)
 OR (NEW.implementation IN ('torznab','newznab') AND NEW.media_type='movies'
    AND typeof(NEW.remove_year)='integer' AND NEW.remove_year IN (0,1) AND NEW.anime_standard_format_search IS NULL)
 OR (NEW.implementation IN ('qbittorrent','sabnzbd','transmission') AND NEW.anime_standard_format_search IS NULL AND NEW.remove_year IS NULL))
BEGIN SELECT RAISE(ABORT,'invalid indexer scope options'); END;
DROP TRIGGER provider_scope_options_update;
CREATE TRIGGER provider_scope_options_update BEFORE UPDATE ON provider_scopes
WHEN NOT ((NEW.implementation IN ('torznab','newznab') AND NEW.media_type='tv'
    AND typeof(NEW.anime_standard_format_search)='integer' AND NEW.anime_standard_format_search IN (0,1) AND NEW.remove_year IS NULL)
 OR (NEW.implementation IN ('torznab','newznab') AND NEW.media_type='movies'
    AND typeof(NEW.remove_year)='integer' AND NEW.remove_year IN (0,1) AND NEW.anime_standard_format_search IS NULL)
 OR (NEW.implementation IN ('qbittorrent','sabnzbd','transmission') AND NEW.anime_standard_format_search IS NULL AND NEW.remove_year IS NULL))
BEGIN SELECT RAISE(ABORT,'invalid indexer scope options'); END;

-- Transmission reuses initial_state for "add paused" (started|stopped); qBittorrent-only layout
-- and piece options stay NULL.
DROP TRIGGER provider_client_options_insert;
CREATE TRIGGER provider_client_options_insert BEFORE INSERT ON provider_scopes
WHEN NOT ((NEW.implementation='qbittorrent'
 AND typeof(NEW.initial_state)='text' AND NEW.initial_state IN ('started','stopped','forced')
 AND typeof(NEW.content_layout)='text' AND NEW.content_layout IN ('default','original','subfolder')
 AND typeof(NEW.sequential_order)='integer' AND NEW.sequential_order IN (0,1)
 AND typeof(NEW.first_last_first)='integer' AND NEW.first_last_first IN (0,1)
 AND typeof(NEW.add_tags)='integer' AND NEW.add_tags IN (0,1) AND (NEW.media_type='tv' OR NEW.add_tags=0))
 OR (NEW.implementation='transmission'
 AND typeof(NEW.initial_state)='text' AND NEW.initial_state IN ('started','stopped')
 AND NEW.content_layout IS NULL AND NEW.sequential_order IS NULL AND NEW.first_last_first IS NULL AND NEW.add_tags IS NULL)
 OR (NEW.implementation IN ('torznab','newznab','sabnzbd') AND NEW.initial_state IS NULL AND NEW.content_layout IS NULL
 AND NEW.sequential_order IS NULL AND NEW.first_last_first IS NULL AND NEW.add_tags IS NULL))
BEGIN SELECT RAISE(ABORT,'invalid client scope options'); END;
DROP TRIGGER provider_client_options_update;
CREATE TRIGGER provider_client_options_update BEFORE UPDATE ON provider_scopes
WHEN NOT ((NEW.implementation='qbittorrent'
 AND typeof(NEW.initial_state)='text' AND NEW.initial_state IN ('started','stopped','forced')
 AND typeof(NEW.content_layout)='text' AND NEW.content_layout IN ('default','original','subfolder')
 AND typeof(NEW.sequential_order)='integer' AND NEW.sequential_order IN (0,1)
 AND typeof(NEW.first_last_first)='integer' AND NEW.first_last_first IN (0,1)
 AND typeof(NEW.add_tags)='integer' AND NEW.add_tags IN (0,1) AND (NEW.media_type='tv' OR NEW.add_tags=0))
 OR (NEW.implementation='transmission'
 AND typeof(NEW.initial_state)='text' AND NEW.initial_state IN ('started','stopped')
 AND NEW.content_layout IS NULL AND NEW.sequential_order IS NULL AND NEW.first_last_first IS NULL AND NEW.add_tags IS NULL)
 OR (NEW.implementation IN ('torznab','newznab','sabnzbd') AND NEW.initial_state IS NULL AND NEW.content_layout IS NULL
 AND NEW.sequential_order IS NULL AND NEW.first_last_first IS NULL AND NEW.add_tags IS NULL))
BEGIN SELECT RAISE(ABORT,'invalid client scope options'); END;

-- Labels compare case-insensitively in the client; TV and movies must never share one.
CREATE UNIQUE INDEX provider_torrent_labels ON provider_scopes(provider_id,lower(category)) WHERE implementation='transmission' AND category IS NOT NULL;

-- Directory placement for a Transmission scope whose category is NULL. Rows are immutable and
-- follow their scope row; writers delete them before replacing scopes.
CREATE TABLE provider_scope_directories (
    provider_id TEXT NOT NULL,
    media_type TEXT NOT NULL CHECK (media_type IN ('tv','movies')),
    implementation TEXT NOT NULL CHECK (implementation='transmission'),
    directory TEXT NOT NULL CHECK (typeof(directory)='text' AND length(CAST(directory AS BLOB)) BETWEEN 2 AND 1024
        AND instr(directory,char(0))=0 AND (substr(directory,1,1)='/' OR (substr(directory,2,1)=':' AND substr(directory,3,1) IN ('/','\')))),
    PRIMARY KEY(provider_id,media_type),
    FOREIGN KEY(provider_id,media_type) REFERENCES provider_scopes(provider_id,media_type) ON DELETE CASCADE
) WITHOUT ROWID;
CREATE TRIGGER provider_scope_directory_insert BEFORE INSERT ON provider_scope_directories BEGIN
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM provider_scopes WHERE provider_id=NEW.provider_id AND media_type=NEW.media_type AND implementation='transmission' AND category IS NULL) THEN RAISE(ABORT,'directory placement requires a category-free transmission scope') END;
 -- Neither domain's directory may equal or contain the other's, compared separator- and
 -- case-insensitively (stored directories have no trailing separator).
 SELECT CASE WHEN EXISTS(SELECT 1 FROM provider_scope_directories d
   WHERE d.provider_id=NEW.provider_id AND d.media_type!=NEW.media_type
   AND (lower(replace(NEW.directory,'\','/'))=lower(replace(d.directory,'\','/'))
     OR substr(lower(replace(NEW.directory,'\','/')),1,length(lower(replace(d.directory,'\','/')))+1)=lower(replace(d.directory,'\','/'))||'/'
     OR substr(lower(replace(d.directory,'\','/')),1,length(lower(replace(NEW.directory,'\','/')))+1)=lower(replace(NEW.directory,'\','/'))||'/'))
  THEN RAISE(ABORT,'conflicting client directory ownership') END;
END;
CREATE TRIGGER provider_scope_directory_immutable BEFORE UPDATE ON provider_scope_directories
BEGIN SELECT RAISE(ABORT,'directory placement is immutable; replace the scope'); END;
CREATE TRIGGER provider_scope_directory_category BEFORE UPDATE OF category ON provider_scopes
WHEN NEW.category IS NOT NULL AND EXISTS(SELECT 1 FROM provider_scope_directories WHERE provider_id=NEW.provider_id AND media_type=NEW.media_type)
BEGIN SELECT RAISE(ABORT,'a scope cannot have both a category and a directory'); END;
-- A directory row changes the scope's observable placement: invalidate tests and observations.
CREATE TRIGGER provider_scope_directory_invalidates AFTER INSERT ON provider_scope_directories BEGIN
 DELETE FROM provider_tests WHERE provider_id=NEW.provider_id;
 DELETE FROM download_refresh_snapshots WHERE provider_id=NEW.provider_id AND media_type=NEW.media_type;
END;
