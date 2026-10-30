-- Provider scope shapes for every registered implementation at once.
--
-- Two lanes replaced the same six provider_scope triggers: the torrent RSS feed lane (0068) added
-- the 'torrentrss' branch, and the label-torrent family lane (0069) added the
-- 'deluge','rtorrent','utorrent','flood','aria2','downloadstation' branch. Whichever ran last
-- dropped the other lane's branch, so the trigger set is reinstalled here with both. The registry
-- rows, the minimum_seeders column (0068) and provider_label_categories (0069) are left alone:
-- only the trigger bodies are replaced, and provider_scopes is not rebuilt (a rebuild would
-- cascade-delete scopes, test results, schedules and journals under foreign_keys=ON).
--
-- A torrent RSS feed is an indexer without a search API, so its scope carries neither categories
-- nor priorities and its search flags are off. A label client is a torrent client whose scope is
-- one lowercase label with a queue placement and an add-paused flag in initial_state.

DROP TRIGGER provider_scope_shape_insert;
CREATE TRIGGER provider_scope_shape_insert BEFORE INSERT ON provider_scopes
WHEN NOT ((NEW.implementation IN ('torznab','newznab') AND NEW.categories IS NOT NULL AND NEW.anime_categories IS NOT NULL
    AND NEW.category IS NULL AND NEW.imported_category IS NULL AND NEW.recent_priority IS NULL AND NEW.older_priority IS NULL
    AND ((NEW.media_type='movies' AND json_array_length(NEW.categories)>0 AND json_array_length(NEW.anime_categories)=0)
      OR (NEW.media_type='tv' AND json_array_length(NEW.categories)+json_array_length(NEW.anime_categories)>0)))
  OR (NEW.implementation='torrentrss' AND NEW.categories IS NULL AND NEW.anime_categories IS NULL AND NEW.category IS NULL
    AND NEW.imported_category IS NULL AND NEW.recent_priority IS NULL AND NEW.older_priority IS NULL)
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
      AND NEW.category NOT GLOB '*[^A-Za-z0-9._-]*' AND NEW.category NOT IN ('.','..'))))
  OR (NEW.implementation='nzbget' AND NEW.categories IS NULL AND NEW.anime_categories IS NULL AND NEW.category IS NOT NULL
    AND NEW.imported_category IS NULL AND NEW.download_client_id IS NULL
    AND typeof(NEW.recent_priority)='integer' AND NEW.recent_priority IN (-100,-2,-1,0,1,2)
    AND typeof(NEW.older_priority)='integer' AND NEW.older_priority IN (-100,-2,-1,0,1,2))
  OR (NEW.implementation IN ('deluge','rtorrent','utorrent','flood','aria2','downloadstation') AND NEW.categories IS NULL AND NEW.anime_categories IS NULL
    AND NEW.imported_category IS NULL AND NEW.download_client_id IS NULL
    AND typeof(NEW.recent_priority)='integer' AND NEW.recent_priority IN (0,1)
    AND typeof(NEW.older_priority)='integer' AND NEW.older_priority IN (0,1)
    AND typeof(NEW.category)='text' AND length(NEW.category) BETWEEN 1 AND 64
    AND NEW.category NOT GLOB '*[^a-z0-9._-]*'))
BEGIN SELECT RAISE(ABORT,'invalid provider scope shape'); END;

DROP TRIGGER provider_scope_shape_update;
CREATE TRIGGER provider_scope_shape_update BEFORE UPDATE ON provider_scopes
WHEN NOT ((NEW.implementation IN ('torznab','newznab') AND NEW.categories IS NOT NULL AND NEW.anime_categories IS NOT NULL
    AND NEW.category IS NULL AND NEW.imported_category IS NULL AND NEW.recent_priority IS NULL AND NEW.older_priority IS NULL
    AND ((NEW.media_type='movies' AND json_array_length(NEW.categories)>0 AND json_array_length(NEW.anime_categories)=0)
      OR (NEW.media_type='tv' AND json_array_length(NEW.categories)+json_array_length(NEW.anime_categories)>0)))
  OR (NEW.implementation='torrentrss' AND NEW.categories IS NULL AND NEW.anime_categories IS NULL AND NEW.category IS NULL
    AND NEW.imported_category IS NULL AND NEW.recent_priority IS NULL AND NEW.older_priority IS NULL)
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
      AND NEW.category NOT GLOB '*[^A-Za-z0-9._-]*' AND NEW.category NOT IN ('.','..'))))
  OR (NEW.implementation='nzbget' AND NEW.categories IS NULL AND NEW.anime_categories IS NULL AND NEW.category IS NOT NULL
    AND NEW.imported_category IS NULL AND NEW.download_client_id IS NULL
    AND typeof(NEW.recent_priority)='integer' AND NEW.recent_priority IN (-100,-2,-1,0,1,2)
    AND typeof(NEW.older_priority)='integer' AND NEW.older_priority IN (-100,-2,-1,0,1,2))
  OR (NEW.implementation IN ('deluge','rtorrent','utorrent','flood','aria2','downloadstation') AND NEW.categories IS NULL AND NEW.anime_categories IS NULL
    AND NEW.imported_category IS NULL AND NEW.download_client_id IS NULL
    AND typeof(NEW.recent_priority)='integer' AND NEW.recent_priority IN (0,1)
    AND typeof(NEW.older_priority)='integer' AND NEW.older_priority IN (0,1)
    AND typeof(NEW.category)='text' AND length(NEW.category) BETWEEN 1 AND 64
    AND NEW.category NOT GLOB '*[^a-z0-9._-]*'))
BEGIN SELECT RAISE(ABORT,'invalid provider scope shape'); END;

DROP TRIGGER provider_scope_options_insert;
CREATE TRIGGER provider_scope_options_insert BEFORE INSERT ON provider_scopes
WHEN NOT ((NEW.implementation IN ('torznab','newznab') AND NEW.media_type='tv'
    AND typeof(NEW.anime_standard_format_search)='integer' AND NEW.anime_standard_format_search IN (0,1) AND NEW.remove_year IS NULL)
 OR (NEW.implementation IN ('torznab','newznab') AND NEW.media_type='movies'
    AND typeof(NEW.remove_year)='integer' AND NEW.remove_year IN (0,1) AND NEW.anime_standard_format_search IS NULL)
 OR (NEW.implementation IN ('qbittorrent','sabnzbd','transmission','nzbget','deluge','rtorrent','utorrent','flood','aria2','downloadstation') AND NEW.anime_standard_format_search IS NULL AND NEW.remove_year IS NULL)
 OR (NEW.implementation='torrentrss' AND NEW.anime_standard_format_search IS NULL AND NEW.remove_year IS NULL
    AND NEW.enable_automatic_search=0 AND NEW.enable_interactive_search=0))
BEGIN SELECT RAISE(ABORT,'invalid indexer scope options'); END;

DROP TRIGGER provider_scope_options_update;
CREATE TRIGGER provider_scope_options_update BEFORE UPDATE ON provider_scopes
WHEN NOT ((NEW.implementation IN ('torznab','newznab') AND NEW.media_type='tv'
    AND typeof(NEW.anime_standard_format_search)='integer' AND NEW.anime_standard_format_search IN (0,1) AND NEW.remove_year IS NULL)
 OR (NEW.implementation IN ('torznab','newznab') AND NEW.media_type='movies'
    AND typeof(NEW.remove_year)='integer' AND NEW.remove_year IN (0,1) AND NEW.anime_standard_format_search IS NULL)
 OR (NEW.implementation IN ('qbittorrent','sabnzbd','transmission','nzbget','deluge','rtorrent','utorrent','flood','aria2','downloadstation') AND NEW.anime_standard_format_search IS NULL AND NEW.remove_year IS NULL)
 OR (NEW.implementation='torrentrss' AND NEW.anime_standard_format_search IS NULL AND NEW.remove_year IS NULL
    AND NEW.enable_automatic_search=0 AND NEW.enable_interactive_search=0))
BEGIN SELECT RAISE(ABORT,'invalid indexer scope options'); END;

-- Label clients reuse initial_state for "add paused" (started|stopped); qBittorrent-only layout
-- and piece options stay NULL.
DROP TRIGGER provider_client_options_insert;
CREATE TRIGGER provider_client_options_insert BEFORE INSERT ON provider_scopes
WHEN NOT ((NEW.implementation='qbittorrent'
 AND typeof(NEW.initial_state)='text' AND NEW.initial_state IN ('started','stopped','forced')
 AND typeof(NEW.content_layout)='text' AND NEW.content_layout IN ('default','original','subfolder')
 AND typeof(NEW.sequential_order)='integer' AND NEW.sequential_order IN (0,1)
 AND typeof(NEW.first_last_first)='integer' AND NEW.first_last_first IN (0,1)
 AND typeof(NEW.add_tags)='integer' AND NEW.add_tags IN (0,1) AND (NEW.media_type='tv' OR NEW.add_tags=0))
 OR (NEW.implementation IN ('transmission','nzbget','deluge','rtorrent','utorrent','flood','aria2','downloadstation')
 AND typeof(NEW.initial_state)='text' AND NEW.initial_state IN ('started','stopped')
 AND NEW.content_layout IS NULL AND NEW.sequential_order IS NULL AND NEW.first_last_first IS NULL AND NEW.add_tags IS NULL)
 OR (NEW.implementation IN ('torznab','newznab','sabnzbd','torrentrss') AND NEW.initial_state IS NULL AND NEW.content_layout IS NULL
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
 OR (NEW.implementation IN ('transmission','nzbget','deluge','rtorrent','utorrent','flood','aria2','downloadstation')
 AND typeof(NEW.initial_state)='text' AND NEW.initial_state IN ('started','stopped')
 AND NEW.content_layout IS NULL AND NEW.sequential_order IS NULL AND NEW.first_last_first IS NULL AND NEW.add_tags IS NULL)
 OR (NEW.implementation IN ('torznab','newznab','sabnzbd','torrentrss') AND NEW.initial_state IS NULL AND NEW.content_layout IS NULL
 AND NEW.sequential_order IS NULL AND NEW.first_last_first IS NULL AND NEW.add_tags IS NULL))
BEGIN SELECT RAISE(ABORT,'invalid client scope options'); END;
