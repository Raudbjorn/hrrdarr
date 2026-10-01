-- Actual scope authority, not nullable enabled fallbacks. Existing constructors retain
-- their prior all-enabled operation modes; download clients never receive indexer modes.
CREATE TABLE provider_indexer_modes (
 provider_id TEXT NOT NULL,
 media_type TEXT NOT NULL CHECK(media_type IN('tv','movies')),
 enable_rss INTEGER NOT NULL DEFAULT 1 CHECK(typeof(enable_rss)='integer' AND enable_rss IN(0,1)),
 enable_automatic_search INTEGER NOT NULL DEFAULT 1 CHECK(typeof(enable_automatic_search)='integer' AND enable_automatic_search IN(0,1)),
 enable_interactive_search INTEGER NOT NULL DEFAULT 1 CHECK(typeof(enable_interactive_search)='integer' AND enable_interactive_search IN(0,1)),
 PRIMARY KEY(provider_id,media_type),
 FOREIGN KEY(provider_id,media_type) REFERENCES provider_scopes(provider_id,media_type) ON DELETE CASCADE
);
-- Insert before observation-invalidation guards: backfill does not change config
-- or destroy accepted revision-bound Test evidence.
INSERT INTO provider_indexer_modes(provider_id,media_type)
 SELECT provider_id,media_type FROM provider_scopes WHERE implementation IN('torznab','newznab');
CREATE TRIGGER provider_modes_owner_insert BEFORE INSERT ON provider_indexer_modes BEGIN
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM provider_scopes WHERE provider_id=NEW.provider_id AND media_type=NEW.media_type AND implementation IN('torznab','newznab')) THEN RAISE(ABORT,'indexer_modes_owner') END;
END;
CREATE TRIGGER provider_modes_owner_update BEFORE UPDATE ON provider_indexer_modes BEGIN
 SELECT CASE WHEN NEW.provider_id IS NOT OLD.provider_id OR NEW.media_type IS NOT OLD.media_type THEN RAISE(ABORT,'indexer_modes_identity') END;
END;
CREATE TRIGGER provider_modes_required BEFORE DELETE ON provider_indexer_modes
 WHEN EXISTS(SELECT 1 FROM provider_scopes WHERE provider_id=OLD.provider_id AND media_type=OLD.media_type)
 BEGIN SELECT RAISE(ABORT,'indexer_modes_required'); END;
CREATE TRIGGER provider_scope_initialize_modes AFTER INSERT ON provider_scopes
 WHEN NEW.implementation IN('torznab','newznab') BEGIN
 INSERT INTO provider_indexer_modes(provider_id,media_type) VALUES(NEW.provider_id,NEW.media_type);
END;
CREATE TRIGGER provider_modes_invalidate_test AFTER UPDATE ON provider_indexer_modes
 WHEN NEW.enable_rss IS NOT OLD.enable_rss OR NEW.enable_automatic_search IS NOT OLD.enable_automatic_search OR NEW.enable_interactive_search IS NOT OLD.enable_interactive_search
 BEGIN DELETE FROM provider_tests WHERE provider_id=NEW.provider_id; END;
-- Modes cannot be orphaned by direct maintenance changing a scope identity.
CREATE TRIGGER provider_scope_authority_identity BEFORE UPDATE OF provider_id,media_type,implementation ON provider_scopes
 WHEN NEW.provider_id IS NOT OLD.provider_id OR NEW.media_type IS NOT OLD.media_type OR NEW.implementation IS NOT OLD.implementation
 BEGIN SELECT RAISE(ABORT,'provider_scope_authority_identity'); END;

CREATE TABLE provider_scope_tags (
 provider_id TEXT NOT NULL,
 media_type TEXT NOT NULL CHECK(media_type IN('tv','movies')),
 tag_id INTEGER NOT NULL,
 PRIMARY KEY(provider_id,media_type,tag_id),
 FOREIGN KEY(provider_id,media_type) REFERENCES provider_scopes(provider_id,media_type) ON DELETE CASCADE,
 FOREIGN KEY(tag_id,media_type) REFERENCES tags(id,media_type) ON DELETE RESTRICT
);
CREATE INDEX provider_scope_tags_tag ON provider_scope_tags(tag_id,provider_id,media_type);
CREATE TRIGGER provider_tags_limit BEFORE INSERT ON provider_scope_tags BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM provider_scope_tags WHERE provider_id=NEW.provider_id AND media_type=NEW.media_type)>=64 THEN RAISE(ABORT,'provider_tag_limit') END;
END;
CREATE TRIGGER provider_tags_identity BEFORE UPDATE ON provider_scope_tags BEGIN
 SELECT RAISE(ABORT,'provider_tag_replace');
END;
CREATE TRIGGER provider_tags_insert_invalidates_test AFTER INSERT ON provider_scope_tags BEGIN
 DELETE FROM provider_tests WHERE provider_id=NEW.provider_id;
END;
CREATE TRIGGER provider_tags_delete_invalidates_test AFTER DELETE ON provider_scope_tags BEGIN
 DELETE FROM provider_tests WHERE provider_id=OLD.provider_id;
END;

CREATE TABLE provider_scope_client_overrides (
 provider_id TEXT NOT NULL,
 media_type TEXT NOT NULL CHECK(media_type IN('tv','movies')),
 client_id TEXT NOT NULL,
 PRIMARY KEY(provider_id,media_type),
 FOREIGN KEY(provider_id,media_type) REFERENCES provider_scopes(provider_id,media_type) ON DELETE CASCADE,
 -- Accepted provider updates delete/recreate scopes in one transaction. Deferral
 -- permits that replacement, but a missing final target still rejects commit.
 FOREIGN KEY(client_id,media_type) REFERENCES provider_scopes(provider_id,media_type) DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX provider_client_override_usage ON provider_scope_client_overrides(client_id,media_type,provider_id);
CREATE TRIGGER provider_override_owner BEFORE INSERT ON provider_scope_client_overrides BEGIN
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM provider_scopes WHERE provider_id=NEW.provider_id AND media_type=NEW.media_type AND implementation='torznab') OR NOT EXISTS(SELECT 1 FROM provider_scopes WHERE provider_id=NEW.client_id AND media_type=NEW.media_type AND implementation='qbittorrent') THEN RAISE(ABORT,'provider_override_protocol_domain') END;
END;
CREATE TRIGGER provider_override_identity BEFORE UPDATE ON provider_scope_client_overrides BEGIN
 SELECT RAISE(ABORT,'provider_override_replace');
END;
CREATE TRIGGER provider_override_insert_invalidates_test AFTER INSERT ON provider_scope_client_overrides BEGIN
 DELETE FROM provider_tests WHERE provider_id=NEW.provider_id;
END;
CREATE TRIGGER provider_override_delete_invalidates_test AFTER DELETE ON provider_scope_client_overrides BEGIN
 DELETE FROM provider_tests WHERE provider_id=OLD.provider_id;
END;

-- One current observation per physical provider, shared by its domain scopes.
-- Configuration changes preserve this row; only completed fenced observations
-- advance it. No failure rows or timestamps are invented by migration.
CREATE TABLE provider_status (
 provider_id TEXT PRIMARY KEY NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
 escalation_level INTEGER NOT NULL CHECK(typeof(escalation_level)='integer' AND escalation_level BETWEEN 0 AND 9),
 status_version INTEGER NOT NULL CHECK(typeof(status_version)='integer' AND status_version BETWEEN 1 AND 9007199254740991),
 last_observed_config_revision INTEGER NOT NULL CHECK(typeof(last_observed_config_revision)='integer' AND last_observed_config_revision BETWEEN 1 AND 9007199254740991),
 initial_failure TEXT CHECK(initial_failure IS NULL OR (typeof(initial_failure)='text' AND length(CAST(initial_failure AS BLOB))=20 AND initial_failure GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9]Z' AND substr(initial_failure,1,4) BETWEEN '0001' AND '9999' AND strftime('%Y-%m-%dT%H:%M:%SZ',julianday(initial_failure)) IS initial_failure)),
 most_recent_failure TEXT CHECK(most_recent_failure IS NULL OR (typeof(most_recent_failure)='text' AND length(CAST(most_recent_failure AS BLOB))=20 AND most_recent_failure GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9]Z' AND substr(most_recent_failure,1,4) BETWEEN '0001' AND '9999' AND strftime('%Y-%m-%dT%H:%M:%SZ',julianday(most_recent_failure)) IS most_recent_failure)),
 disabled_until TEXT CHECK(disabled_until IS NULL OR (typeof(disabled_until)='text' AND length(CAST(disabled_until AS BLOB))=20 AND disabled_until GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9]Z' AND substr(disabled_until,1,4) BETWEEN '0001' AND '9999' AND strftime('%Y-%m-%dT%H:%M:%SZ',julianday(disabled_until)) IS disabled_until)),
 CHECK((initial_failure IS NULL)=(most_recent_failure IS NULL)),
 CHECK(escalation_level=0 OR initial_failure IS NOT NULL),
 CHECK(disabled_until IS NULL OR (initial_failure IS NOT NULL AND escalation_level>0))
);
CREATE TRIGGER provider_status_insert BEFORE INSERT ON provider_status BEGIN
 SELECT CASE WHEN NEW.status_version!=1 THEN RAISE(ABORT,'provider_status_version') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM providers WHERE id=NEW.provider_id AND revision=NEW.last_observed_config_revision AND (implementation IN('torznab','newznab') OR (implementation='qbittorrent' AND NEW.escalation_level<=5))) THEN RAISE(ABORT,'provider_status_owner_revision') END;
END;
CREATE TRIGGER provider_status_update BEFORE UPDATE ON provider_status BEGIN
 SELECT CASE WHEN NEW.provider_id IS NOT OLD.provider_id OR OLD.status_version=9007199254740991 OR NEW.status_version!=OLD.status_version+1 THEN RAISE(ABORT,'provider_status_version') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM providers WHERE id=NEW.provider_id AND revision=NEW.last_observed_config_revision AND (implementation IN('torznab','newznab') OR (implementation='qbittorrent' AND NEW.escalation_level<=5))) THEN RAISE(ABORT,'provider_status_owner_revision') END;
END;

CREATE TABLE provider_selection_cursors (
 media_type TEXT NOT NULL CHECK(media_type IN('tv','movies')),
 protocol TEXT NOT NULL CHECK(protocol IN('torrent','usenet')),
 -- No FK: deletion preserves a historical ordering position, never authority.
 last_client_id TEXT NOT NULL CHECK(length(last_client_id)=36 AND substr(last_client_id,9,1)='-' AND substr(last_client_id,14,1)='-' AND substr(last_client_id,19,1)='-' AND substr(last_client_id,24,1)='-' AND length(replace(last_client_id,'-',''))=32 AND replace(last_client_id,'-','') NOT GLOB '*[^0-9a-f]*'),
 revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision BETWEEN 1 AND 9007199254740991),
 PRIMARY KEY(media_type,protocol)
);
CREATE TRIGGER provider_cursor_insert BEFORE INSERT ON provider_selection_cursors BEGIN
 SELECT CASE WHEN NEW.revision!=1 THEN RAISE(ABORT,'provider_cursor_revision') END;
 SELECT CASE WHEN NEW.protocol!='torrent' OR NOT EXISTS(SELECT 1 FROM provider_scopes WHERE provider_id=NEW.last_client_id AND media_type=NEW.media_type AND implementation='qbittorrent') THEN RAISE(ABORT,'provider_cursor_client') END;
END;
CREATE TRIGGER provider_cursor_update BEFORE UPDATE ON provider_selection_cursors BEGIN
 SELECT CASE WHEN NEW.media_type IS NOT OLD.media_type OR NEW.protocol IS NOT OLD.protocol OR OLD.revision=9007199254740991 OR NEW.revision!=OLD.revision+1 THEN RAISE(ABORT,'provider_cursor_revision') END;
 SELECT CASE WHEN NEW.protocol!='torrent' OR NOT EXISTS(SELECT 1 FROM provider_scopes WHERE provider_id=NEW.last_client_id AND media_type=NEW.media_type AND implementation='qbittorrent') THEN RAISE(ABORT,'provider_cursor_client') END;
END;
ALTER TABLE snapshot_imports ADD COLUMN provider_authority_version INTEGER NOT NULL DEFAULT 0 CHECK(typeof(provider_authority_version)='integer' AND provider_authority_version IN(0,1));
