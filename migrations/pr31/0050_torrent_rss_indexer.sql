-- Generic torrent RSS feed indexer (implementation 'torrentrss').
-- Storage shape: provider_scopes carries no categories/client columns for the new implementation,
-- forces both search flags to 0 (a plain feed has no search API) and adds one nullable
-- minimum_seeders column. download_client_id remains NULL for torrentrss (RSS targets pair an
-- indexer and a client explicitly), search_commands_admit is deliberately unchanged, and
-- no health_checks rows are added.
--
-- providers.implementation and the provider_scopes table CHECK are widened in place. Rebuilding
-- providers is not possible here: the migration transaction runs with foreign_keys=ON, so
-- DROP TABLE providers would cascade-delete scopes, test results, schedules and journals.
-- Widening a CHECK list is the documented writable_schema edit: every existing row already
-- satisfies the wider predicate, so no data scan or rewrite is needed. The guard table fails the
-- whole migration (and rolls it back) if either replacement did not apply.
ALTER TABLE provider_scopes ADD COLUMN minimum_seeders INTEGER DEFAULT NULL CHECK (
    minimum_seeders IS NULL OR (implementation='torrentrss' AND typeof(minimum_seeders)='integer' AND minimum_seeders BETWEEN 0 AND 1000000)
);
PRAGMA writable_schema=ON;
UPDATE sqlite_schema SET sql=replace(sql,
  'CHECK (implementation IN (''torznab'',''newznab'',''qbittorrent''))',
  'CHECK (implementation IN (''torznab'',''newznab'',''qbittorrent'',''torrentrss''))')
 WHERE type='table' AND name='providers';
UPDATE sqlite_schema SET sql=replace(sql,
  'AND recent_priority IS NOT NULL AND older_priority IS NOT NULL))',
  'AND recent_priority IS NOT NULL AND older_priority IS NOT NULL)
      OR (implementation=''torrentrss'' AND categories IS NULL AND anime_categories IS NULL AND category IS NULL AND imported_category IS NULL AND recent_priority IS NULL AND older_priority IS NULL))')
 WHERE type='table' AND name='provider_scopes';
PRAGMA writable_schema=RESET;
CREATE TABLE torrent_rss_migration_guard (valid INTEGER NOT NULL CHECK(valid=1));
INSERT INTO torrent_rss_migration_guard
 SELECT (SELECT count(*) FROM sqlite_schema WHERE type='table' AND name='providers' AND instr(sql,'''qbittorrent'',''torrentrss''))')>0)=1
    AND (SELECT count(*) FROM sqlite_schema WHERE type='table' AND name='provider_scopes' AND instr(sql,'implementation=''torrentrss'' AND categories IS NULL')>0 AND instr(sql,'minimum_seeders INTEGER')>0)=1;
DROP TABLE torrent_rss_migration_guard;
DROP TRIGGER IF EXISTS rss_commands_admit;
CREATE TRIGGER rss_commands_admit BEFORE INSERT ON rss_commands BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 OR NEW.fetch_complete!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT (EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.indexer_id AND p.revision=NEW.indexer_revision AND p.implementation IN ('torznab','newznab','torrentrss') AND s.media_type=NEW.media_type AND p.enabled=1) AND EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.client_id AND p.revision=NEW.client_revision AND p.implementation='qbittorrent' AND s.media_type=NEW.media_type AND p.enabled=1)) THEN RAISE(ABORT,'invalid RSS provider scope or revision') END;
END;
DROP TRIGGER IF EXISTS rss_schedules_admit;
CREATE TRIGGER rss_schedules_admit BEFORE INSERT ON rss_schedules BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM rss_schedules)>=64 THEN RAISE(ABORT,'RSS schedule capacity reached') END;
 SELECT CASE WHEN NOT (EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.indexer_id AND p.revision=NEW.indexer_revision AND p.implementation IN ('torznab','newznab','torrentrss') AND s.media_type=NEW.media_type AND (NEW.enabled=0 OR p.enabled=1)) AND EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.client_id AND p.revision=NEW.client_revision AND p.implementation='qbittorrent' AND s.media_type=NEW.media_type AND (NEW.enabled=0 OR p.enabled=1))) THEN RAISE(ABORT,'invalid RSS schedule provider scope or revision') END;
END;
DROP TRIGGER IF EXISTS rss_schedules_update;
CREATE TRIGGER rss_schedules_update BEFORE UPDATE ON rss_schedules BEGIN
 SELECT CASE WHEN NEW.id IS NOT OLD.id OR NEW.media_type IS NOT OLD.media_type OR NEW.indexer_id IS NOT OLD.indexer_id OR NEW.client_id IS NOT OLD.client_id OR NEW.created_at IS NOT OLD.created_at THEN RAISE(ABORT,'RSS schedule identity is immutable') END;
 SELECT CASE WHEN (NEW.enabled=1 OR NEW.indexer_revision IS NOT OLD.indexer_revision OR NEW.client_revision IS NOT OLD.client_revision) AND NOT (EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.indexer_id AND p.revision=NEW.indexer_revision AND p.implementation IN ('torznab','newznab','torrentrss') AND s.media_type=NEW.media_type AND (NEW.enabled=0 OR p.enabled=1)) AND EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.client_id AND p.revision=NEW.client_revision AND p.implementation='qbittorrent' AND s.media_type=NEW.media_type AND (NEW.enabled=0 OR p.enabled=1))) THEN RAISE(ABORT,'invalid RSS schedule provider scope or revision') END;
END;
DROP TRIGGER IF EXISTS provider_test_insert_owner;
CREATE TRIGGER provider_test_insert_owner BEFORE INSERT ON provider_tests
WHEN NOT EXISTS(SELECT 1 FROM providers WHERE id=NEW.provider_id AND revision=NEW.config_revision AND implementation IN ('torznab','newznab','qbittorrent','torrentrss'))
BEGIN SELECT RAISE(ABORT,'provider test revision or implementation mismatch'); END;
DROP TRIGGER IF EXISTS provider_test_update_owner;
CREATE TRIGGER provider_test_update_owner BEFORE UPDATE ON provider_tests
WHEN NEW.provider_id IS NOT OLD.provider_id OR NOT EXISTS(SELECT 1 FROM providers WHERE id=NEW.provider_id AND revision=NEW.config_revision AND implementation IN ('torznab','newznab','qbittorrent','torrentrss'))
BEGIN SELECT RAISE(ABORT,'provider test revision or implementation mismatch'); END;
DROP TRIGGER IF EXISTS provider_scope_options_insert;
CREATE TRIGGER provider_scope_options_insert BEFORE INSERT ON provider_scopes
WHEN NOT ((NEW.implementation IN ('torznab','newznab') AND NEW.media_type='tv'
    AND typeof(NEW.anime_standard_format_search)='integer' AND NEW.anime_standard_format_search IN (0,1) AND NEW.remove_year IS NULL)
 OR (NEW.implementation IN ('torznab','newznab') AND NEW.media_type='movies'
    AND typeof(NEW.remove_year)='integer' AND NEW.remove_year IN (0,1) AND NEW.anime_standard_format_search IS NULL)
 OR (NEW.implementation='qbittorrent' AND NEW.anime_standard_format_search IS NULL AND NEW.remove_year IS NULL)
 OR (NEW.implementation='torrentrss' AND NEW.anime_standard_format_search IS NULL AND NEW.remove_year IS NULL
    AND NEW.enable_automatic_search=0 AND NEW.enable_interactive_search=0))
BEGIN SELECT RAISE(ABORT,'invalid indexer scope options'); END;
DROP TRIGGER IF EXISTS provider_scope_options_update;
CREATE TRIGGER provider_scope_options_update BEFORE UPDATE ON provider_scopes
WHEN NOT ((NEW.implementation IN ('torznab','newznab') AND NEW.media_type='tv'
    AND typeof(NEW.anime_standard_format_search)='integer' AND NEW.anime_standard_format_search IN (0,1) AND NEW.remove_year IS NULL)
 OR (NEW.implementation IN ('torznab','newznab') AND NEW.media_type='movies'
    AND typeof(NEW.remove_year)='integer' AND NEW.remove_year IN (0,1) AND NEW.anime_standard_format_search IS NULL)
 OR (NEW.implementation='qbittorrent' AND NEW.anime_standard_format_search IS NULL AND NEW.remove_year IS NULL)
 OR (NEW.implementation='torrentrss' AND NEW.anime_standard_format_search IS NULL AND NEW.remove_year IS NULL
    AND NEW.enable_automatic_search=0 AND NEW.enable_interactive_search=0))
BEGIN SELECT RAISE(ABORT,'invalid indexer scope options'); END;
DROP TRIGGER IF EXISTS provider_client_options_insert;
CREATE TRIGGER provider_client_options_insert BEFORE INSERT ON provider_scopes
WHEN NOT ((NEW.implementation='qbittorrent'
 AND typeof(NEW.initial_state)='text' AND NEW.initial_state IN ('started','stopped','forced')
 AND typeof(NEW.content_layout)='text' AND NEW.content_layout IN ('default','original','subfolder')
 AND typeof(NEW.sequential_order)='integer' AND NEW.sequential_order IN (0,1)
 AND typeof(NEW.first_last_first)='integer' AND NEW.first_last_first IN (0,1)
 AND typeof(NEW.add_tags)='integer' AND NEW.add_tags IN (0,1) AND (NEW.media_type='tv' OR NEW.add_tags=0))
 OR (NEW.implementation IN ('torznab','newznab','torrentrss') AND NEW.initial_state IS NULL AND NEW.content_layout IS NULL
 AND NEW.sequential_order IS NULL AND NEW.first_last_first IS NULL AND NEW.add_tags IS NULL))
BEGIN SELECT RAISE(ABORT,'invalid client scope options'); END;
DROP TRIGGER IF EXISTS provider_client_options_update;
CREATE TRIGGER provider_client_options_update BEFORE UPDATE ON provider_scopes
WHEN NOT ((NEW.implementation='qbittorrent'
 AND typeof(NEW.initial_state)='text' AND NEW.initial_state IN ('started','stopped','forced')
 AND typeof(NEW.content_layout)='text' AND NEW.content_layout IN ('default','original','subfolder')
 AND typeof(NEW.sequential_order)='integer' AND NEW.sequential_order IN (0,1)
 AND typeof(NEW.first_last_first)='integer' AND NEW.first_last_first IN (0,1)
 AND typeof(NEW.add_tags)='integer' AND NEW.add_tags IN (0,1) AND (NEW.media_type='tv' OR NEW.add_tags=0))
 OR (NEW.implementation IN ('torznab','newznab','torrentrss') AND NEW.initial_state IS NULL AND NEW.content_layout IS NULL
 AND NEW.sequential_order IS NULL AND NEW.first_last_first IS NULL AND NEW.add_tags IS NULL))
BEGIN SELECT RAISE(ABORT,'invalid client scope options'); END;
