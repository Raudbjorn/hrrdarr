-- Scoped client defaults; indexer scopes retain no client settings.
ALTER TABLE provider_scopes ADD COLUMN initial_state TEXT;
ALTER TABLE provider_scopes ADD COLUMN content_layout TEXT;
ALTER TABLE provider_scopes ADD COLUMN sequential_order INTEGER;
ALTER TABLE provider_scopes ADD COLUMN first_last_first INTEGER;
ALTER TABLE provider_scopes ADD COLUMN add_tags INTEGER;
-- Semantic defaults preserve prior observations; all DDL and backfill are transactional.
DROP TRIGGER provider_scope_update_invalidates_test;
UPDATE provider_scopes SET initial_state='started',content_layout='default',sequential_order=0,first_last_first=0,add_tags=0 WHERE implementation='qbittorrent';
CREATE TRIGGER provider_client_options_insert BEFORE INSERT ON provider_scopes
WHEN NOT ((NEW.implementation='qbittorrent'
 AND typeof(NEW.initial_state)='text' AND NEW.initial_state IN ('started','stopped','forced')
 AND typeof(NEW.content_layout)='text' AND NEW.content_layout IN ('default','original','subfolder')
 AND typeof(NEW.sequential_order)='integer' AND NEW.sequential_order IN (0,1)
 AND typeof(NEW.first_last_first)='integer' AND NEW.first_last_first IN (0,1)
 AND typeof(NEW.add_tags)='integer' AND NEW.add_tags IN (0,1) AND (NEW.media_type='tv' OR NEW.add_tags=0))
 OR (NEW.implementation IN ('torznab','newznab') AND NEW.initial_state IS NULL AND NEW.content_layout IS NULL
 AND NEW.sequential_order IS NULL AND NEW.first_last_first IS NULL AND NEW.add_tags IS NULL))
BEGIN SELECT RAISE(ABORT,'invalid client scope options'); END;
CREATE TRIGGER provider_client_options_update BEFORE UPDATE ON provider_scopes
WHEN NOT ((NEW.implementation='qbittorrent'
 AND typeof(NEW.initial_state)='text' AND NEW.initial_state IN ('started','stopped','forced')
 AND typeof(NEW.content_layout)='text' AND NEW.content_layout IN ('default','original','subfolder')
 AND typeof(NEW.sequential_order)='integer' AND NEW.sequential_order IN (0,1)
 AND typeof(NEW.first_last_first)='integer' AND NEW.first_last_first IN (0,1)
 AND typeof(NEW.add_tags)='integer' AND NEW.add_tags IN (0,1) AND (NEW.media_type='tv' OR NEW.add_tags=0))
 OR (NEW.implementation IN ('torznab','newznab') AND NEW.initial_state IS NULL AND NEW.content_layout IS NULL
 AND NEW.sequential_order IS NULL AND NEW.first_last_first IS NULL AND NEW.add_tags IS NULL))
BEGIN SELECT RAISE(ABORT,'invalid client scope options'); END;
CREATE TRIGGER provider_client_ownership_insert BEFORE INSERT ON provider_scopes
WHEN NEW.implementation='qbittorrent' AND EXISTS (
 SELECT 1 FROM provider_scopes s,
 json_each(json_array(s.category,s.imported_category)) existing,
 json_each(json_array(NEW.category,NEW.imported_category)) proposed
 WHERE s.provider_id=NEW.provider_id AND s.media_type!=NEW.media_type AND s.implementation='qbittorrent'
 AND existing.value IS NOT NULL AND proposed.value IS NOT NULL
 AND (existing.value=proposed.value
 OR substr(existing.value,1,length(proposed.value)+1)=proposed.value||'/'
 OR substr(proposed.value,1,length(existing.value)+1)=existing.value||'/'))
BEGIN SELECT RAISE(ABORT,'conflicting client category ownership'); END;
CREATE TRIGGER provider_client_ownership_update BEFORE UPDATE ON provider_scopes
WHEN NEW.implementation='qbittorrent' AND EXISTS (
 SELECT 1 FROM provider_scopes s,
 json_each(json_array(s.category,s.imported_category)) existing,
 json_each(json_array(NEW.category,NEW.imported_category)) proposed
 WHERE s.provider_id=NEW.provider_id AND s.media_type!=NEW.media_type AND s.implementation='qbittorrent'
 AND existing.value IS NOT NULL AND proposed.value IS NOT NULL
 AND (existing.value=proposed.value
 OR substr(existing.value,1,length(proposed.value)+1)=proposed.value||'/'
 OR substr(proposed.value,1,length(existing.value)+1)=existing.value||'/'))
BEGIN SELECT RAISE(ABORT,'conflicting client category ownership'); END;
UPDATE provider_scopes SET category=category WHERE implementation='qbittorrent';
CREATE TRIGGER provider_scope_update_invalidates_test AFTER UPDATE ON provider_scopes
BEGIN DELETE FROM provider_tests WHERE provider_id=OLD.provider_id OR provider_id=NEW.provider_id; END;
DROP TRIGGER provider_test_insert_owner;
DROP TRIGGER provider_test_update_owner;
CREATE TRIGGER provider_test_insert_owner BEFORE INSERT ON provider_tests
WHEN NOT EXISTS(SELECT 1 FROM providers WHERE id=NEW.provider_id AND revision=NEW.config_revision AND implementation IN ('torznab','newznab','qbittorrent'))
BEGIN SELECT RAISE(ABORT,'provider test revision or implementation mismatch'); END;
CREATE TRIGGER provider_test_update_owner BEFORE UPDATE ON provider_tests
WHEN NEW.provider_id IS NOT OLD.provider_id OR NOT EXISTS(SELECT 1 FROM providers WHERE id=NEW.provider_id AND revision=NEW.config_revision AND implementation IN ('torznab','newznab','qbittorrent'))
BEGIN SELECT RAISE(ABORT,'provider test revision or implementation mismatch'); END;
