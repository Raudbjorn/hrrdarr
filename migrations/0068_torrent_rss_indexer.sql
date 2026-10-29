-- Generic torrent RSS feed indexer (implementation 'torrentrss').
-- Authored on origin/main as migration 0050 against the pre-registry schema; re-expressed here as the
-- final step (0068) against the 0059 download-client registry shape that parity/loop builds on.
-- Storage shape: provider_scopes carries no categories/priorities/client columns for the new
-- implementation, forces both search flags to 0 (a plain feed has no search API) and gains one
-- nullable minimum_seeders column. download_client_id remains NULL for torrentrss (the provider_scopes
-- CHECK only admits torznab/newznab bindings), search_commands_admit is deliberately unchanged
-- (a feed cannot be searched), indexer_status/backoff is not extended, and no health_checks rows are added.
--
-- The registry makes this additive: one implementation row plus replacement shape/option/admission
-- triggers. Neither providers nor provider_scopes are rebuilt (a rebuild would cascade-delete scopes,
-- test results, schedules and journals under foreign_keys=ON).
INSERT INTO provider_implementations(implementation,kind,protocol) VALUES ('torrentrss','indexer','torrent');
ALTER TABLE provider_scopes ADD COLUMN minimum_seeders INTEGER DEFAULT NULL CHECK (
    minimum_seeders IS NULL OR (implementation='torrentrss' AND typeof(minimum_seeders)='integer' AND minimum_seeders BETWEEN 0 AND 1000000)
);

DROP TRIGGER rss_schedules_admit;
CREATE TRIGGER rss_schedules_admit BEFORE INSERT ON rss_schedules BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM rss_schedules)>=64 THEN RAISE(ABORT,'RSS schedule capacity reached') END;
 SELECT CASE WHEN NOT (EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.indexer_id AND p.revision=NEW.indexer_revision AND p.implementation IN ('torznab','newznab','torrentrss') AND s.media_type=NEW.media_type AND (NEW.enabled=0 OR p.enabled=1)) AND (NEW.selection_kind='default' OR EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.client_id AND p.revision=NEW.client_revision AND p.implementation IN (SELECT c.implementation FROM provider_implementations c JOIN provider_implementations i ON i.protocol=c.protocol JOIN providers x ON x.implementation=i.implementation WHERE c.kind='download_client' AND x.id=NEW.indexer_id) AND s.media_type=NEW.media_type AND (NEW.enabled=0 OR p.enabled=1)))) THEN RAISE(ABORT,'invalid RSS schedule provider scope or revision') END;
END;

DROP TRIGGER rss_schedules_update;
CREATE TRIGGER rss_schedules_update BEFORE UPDATE ON rss_schedules BEGIN
 SELECT CASE WHEN NEW.selection_kind IS NOT OLD.selection_kind OR NEW.id IS NOT OLD.id OR NEW.media_type IS NOT OLD.media_type OR NEW.indexer_id IS NOT OLD.indexer_id OR NEW.client_id IS NOT OLD.client_id OR NEW.created_at IS NOT OLD.created_at THEN RAISE(ABORT,'RSS schedule identity is immutable') END;
 SELECT CASE WHEN (NEW.enabled=1 OR NEW.indexer_revision IS NOT OLD.indexer_revision OR NEW.client_revision IS NOT OLD.client_revision) AND NOT (EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.indexer_id AND p.revision=NEW.indexer_revision AND p.implementation IN ('torznab','newznab','torrentrss') AND s.media_type=NEW.media_type AND (NEW.enabled=0 OR p.enabled=1)) AND (NEW.selection_kind='default' OR EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.client_id AND p.revision=NEW.client_revision AND p.implementation IN (SELECT c.implementation FROM provider_implementations c JOIN provider_implementations i ON i.protocol=c.protocol JOIN providers x ON x.implementation=i.implementation WHERE c.kind='download_client' AND x.id=NEW.indexer_id) AND s.media_type=NEW.media_type AND (NEW.enabled=0 OR p.enabled=1)))) THEN RAISE(ABORT,'invalid RSS schedule provider scope or revision') END;
END;

DROP TRIGGER rss_commands_admit;
CREATE TRIGGER rss_commands_admit BEFORE INSERT ON rss_commands BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM recycle_bin_cleanup_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rename_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 OR NEW.fetch_complete!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT (EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.indexer_id AND p.revision=NEW.indexer_revision AND p.implementation IN ('torznab','newznab','torrentrss') AND s.media_type=NEW.media_type AND p.enabled=1) AND (NEW.selection_kind='default' OR EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.client_id AND p.revision=NEW.client_revision AND p.implementation IN (SELECT c.implementation FROM provider_implementations c JOIN provider_implementations i ON i.protocol=c.protocol JOIN providers x ON x.implementation=i.implementation WHERE c.kind='download_client' AND x.id=NEW.indexer_id) AND s.media_type=NEW.media_type AND p.enabled=1))) THEN RAISE(ABORT,'invalid RSS provider scope or revision') END;
END;

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
    AND typeof(NEW.older_priority)='integer' AND NEW.older_priority IN (-100,-2,-1,0,1,2)))
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
    AND typeof(NEW.older_priority)='integer' AND NEW.older_priority IN (-100,-2,-1,0,1,2)))
BEGIN SELECT RAISE(ABORT,'invalid provider scope shape'); END;

DROP TRIGGER provider_scope_options_insert;
CREATE TRIGGER provider_scope_options_insert BEFORE INSERT ON provider_scopes
WHEN NOT ((NEW.implementation IN ('torznab','newznab') AND NEW.media_type='tv'
    AND typeof(NEW.anime_standard_format_search)='integer' AND NEW.anime_standard_format_search IN (0,1) AND NEW.remove_year IS NULL)
 OR (NEW.implementation IN ('torznab','newznab') AND NEW.media_type='movies'
    AND typeof(NEW.remove_year)='integer' AND NEW.remove_year IN (0,1) AND NEW.anime_standard_format_search IS NULL)
 OR (NEW.implementation IN ('qbittorrent','sabnzbd','transmission','nzbget') AND NEW.anime_standard_format_search IS NULL AND NEW.remove_year IS NULL)
 OR (NEW.implementation='torrentrss' AND NEW.anime_standard_format_search IS NULL AND NEW.remove_year IS NULL
    AND NEW.enable_automatic_search=0 AND NEW.enable_interactive_search=0))
BEGIN SELECT RAISE(ABORT,'invalid indexer scope options'); END;

DROP TRIGGER provider_scope_options_update;
CREATE TRIGGER provider_scope_options_update BEFORE UPDATE ON provider_scopes
WHEN NOT ((NEW.implementation IN ('torznab','newznab') AND NEW.media_type='tv'
    AND typeof(NEW.anime_standard_format_search)='integer' AND NEW.anime_standard_format_search IN (0,1) AND NEW.remove_year IS NULL)
 OR (NEW.implementation IN ('torznab','newznab') AND NEW.media_type='movies'
    AND typeof(NEW.remove_year)='integer' AND NEW.remove_year IN (0,1) AND NEW.anime_standard_format_search IS NULL)
 OR (NEW.implementation IN ('qbittorrent','sabnzbd','transmission','nzbget') AND NEW.anime_standard_format_search IS NULL AND NEW.remove_year IS NULL)
 OR (NEW.implementation='torrentrss' AND NEW.anime_standard_format_search IS NULL AND NEW.remove_year IS NULL
    AND NEW.enable_automatic_search=0 AND NEW.enable_interactive_search=0))
BEGIN SELECT RAISE(ABORT,'invalid indexer scope options'); END;

DROP TRIGGER provider_client_options_insert;
CREATE TRIGGER provider_client_options_insert BEFORE INSERT ON provider_scopes
WHEN NOT ((NEW.implementation='qbittorrent'
 AND typeof(NEW.initial_state)='text' AND NEW.initial_state IN ('started','stopped','forced')
 AND typeof(NEW.content_layout)='text' AND NEW.content_layout IN ('default','original','subfolder')
 AND typeof(NEW.sequential_order)='integer' AND NEW.sequential_order IN (0,1)
 AND typeof(NEW.first_last_first)='integer' AND NEW.first_last_first IN (0,1)
 AND typeof(NEW.add_tags)='integer' AND NEW.add_tags IN (0,1) AND (NEW.media_type='tv' OR NEW.add_tags=0))
 OR (NEW.implementation IN ('transmission','nzbget')
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
 OR (NEW.implementation IN ('transmission','nzbget')
 AND typeof(NEW.initial_state)='text' AND NEW.initial_state IN ('started','stopped')
 AND NEW.content_layout IS NULL AND NEW.sequential_order IS NULL AND NEW.first_last_first IS NULL AND NEW.add_tags IS NULL)
 OR (NEW.implementation IN ('torznab','newznab','sabnzbd','torrentrss') AND NEW.initial_state IS NULL AND NEW.content_layout IS NULL
 AND NEW.sequential_order IS NULL AND NEW.first_last_first IS NULL AND NEW.add_tags IS NULL))
BEGIN SELECT RAISE(ABORT,'invalid client scope options'); END;
