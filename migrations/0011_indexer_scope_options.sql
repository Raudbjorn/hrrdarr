-- False preserves the existing search behavior; client and opposite-domain settings stay absent.
ALTER TABLE provider_scopes ADD COLUMN anime_standard_format_search INTEGER;
ALTER TABLE provider_scopes ADD COLUMN remove_year INTEGER;
-- Backfilling an implicit false is not a configuration change. Preserve prior observations.
-- The migration runner encloses DDL, data and trigger restoration in one transaction.
DROP TRIGGER provider_scope_update_invalidates_test;
UPDATE provider_scopes SET anime_standard_format_search=0 WHERE implementation IN ('torznab','newznab') AND media_type='tv';
UPDATE provider_scopes SET remove_year=0 WHERE implementation IN ('torznab','newznab') AND media_type='movies';
CREATE TRIGGER provider_scope_update_invalidates_test AFTER UPDATE ON provider_scopes
BEGIN DELETE FROM provider_tests WHERE provider_id=OLD.provider_id OR provider_id=NEW.provider_id; END;
CREATE TRIGGER provider_scope_options_insert BEFORE INSERT ON provider_scopes
WHEN NOT ((NEW.implementation IN ('torznab','newznab') AND NEW.media_type='tv'
    AND typeof(NEW.anime_standard_format_search)='integer' AND NEW.anime_standard_format_search IN (0,1) AND NEW.remove_year IS NULL)
 OR (NEW.implementation IN ('torznab','newznab') AND NEW.media_type='movies'
    AND typeof(NEW.remove_year)='integer' AND NEW.remove_year IN (0,1) AND NEW.anime_standard_format_search IS NULL)
 OR (NEW.implementation='qbittorrent' AND NEW.anime_standard_format_search IS NULL AND NEW.remove_year IS NULL))
BEGIN SELECT RAISE(ABORT,'invalid indexer scope options'); END;
CREATE TRIGGER provider_scope_options_update BEFORE UPDATE ON provider_scopes
WHEN NOT ((NEW.implementation IN ('torznab','newznab') AND NEW.media_type='tv'
    AND typeof(NEW.anime_standard_format_search)='integer' AND NEW.anime_standard_format_search IN (0,1) AND NEW.remove_year IS NULL)
 OR (NEW.implementation IN ('torznab','newznab') AND NEW.media_type='movies'
    AND typeof(NEW.remove_year)='integer' AND NEW.remove_year IN (0,1) AND NEW.anime_standard_format_search IS NULL)
 OR (NEW.implementation='qbittorrent' AND NEW.anime_standard_format_search IS NULL AND NEW.remove_year IS NULL))
BEGIN SELECT RAISE(ABORT,'invalid indexer scope options'); END;
