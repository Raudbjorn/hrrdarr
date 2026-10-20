-- Protocol-aware download-client registry. Later client implementations add one registry row and
-- replace the scope shape/option triggers below; neither providers nor provider_scopes need rebuilding.
-- The owning migration connection disables foreign keys before its transaction; every row and
-- relationship is checked before commit and application connections keep enforcement on.
CREATE TABLE provider_implementations (
 implementation TEXT PRIMARY KEY NOT NULL CHECK(length(implementation) BETWEEN 1 AND 32 AND implementation NOT GLOB '*[^a-z0-9_]*'),
 kind TEXT NOT NULL CHECK(kind IN ('indexer','download_client')),
 protocol TEXT NOT NULL CHECK(protocol IN ('torrent','usenet'))
) WITHOUT ROWID;
INSERT INTO provider_implementations(implementation,kind,protocol) VALUES
 ('torznab','indexer','torrent'),('newznab','indexer','usenet'),
 ('qbittorrent','download_client','torrent'),('sabnzbd','download_client','usenet');
CREATE TRIGGER provider_implementation_immutable BEFORE UPDATE ON provider_implementations BEGIN SELECT RAISE(ABORT,'provider implementation registry is immutable'); END;
CREATE TRIGGER provider_implementation_retained BEFORE DELETE ON provider_implementations BEGIN SELECT RAISE(ABORT,'provider implementation registry is immutable'); END;

CREATE TEMP TABLE _registry58_providers AS SELECT * FROM providers;
CREATE TEMP TABLE _registry58_scopes AS SELECT * FROM provider_scopes;
DROP VIEW cdh_scope_authority;
DROP TABLE provider_scopes;
DROP TABLE providers;

CREATE TABLE providers (
    id TEXT PRIMARY KEY NOT NULL CHECK (length(id)=36 AND substr(id,9,1)='-' AND substr(id,14,1)='-' AND substr(id,19,1)='-' AND substr(id,24,1)='-' AND length(replace(id,'-',''))=32 AND replace(id,'-','') NOT GLOB '*[^0-9a-f]*'),
    implementation TEXT NOT NULL REFERENCES provider_implementations(implementation),
    name TEXT NOT NULL CHECK (length(trim(name))>0 AND length(CAST(name AS BLOB))<=128),
    enabled INTEGER NOT NULL CHECK (enabled IN (0,1)),
    priority INTEGER NOT NULL CHECK (typeof(priority)='integer' AND priority BETWEEN 1 AND 100),
    revision INTEGER NOT NULL CHECK (typeof(revision)='integer' AND revision BETWEEN 1 AND 9007199254740991),
    settings_version INTEGER NOT NULL CHECK (settings_version=1),
    endpoint TEXT NOT NULL CHECK (length(trim(endpoint))>0 AND length(CAST(endpoint AS BLOB))<=2048),
    credentials BLOB CHECK (credentials IS NULL OR (typeof(credentials)='blob' AND length(credentials) BETWEEN 29 AND 16384)),
    UNIQUE(id,implementation)
);
INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint,credentials) SELECT id,implementation,name,enabled,priority,revision,settings_version,endpoint,credentials FROM _registry58_providers;
CREATE TABLE provider_scopes (
    provider_id TEXT NOT NULL,
    implementation TEXT NOT NULL,
    media_type TEXT NOT NULL CHECK (media_type IN ('tv','movies')),
    categories TEXT CHECK (categories IS NULL OR (length(CAST(categories AS BLOB))<=4096 AND json_valid(categories) AND json_type(categories)='array' AND json_array_length(categories)<=64)),
    anime_categories TEXT CHECK (anime_categories IS NULL OR (length(CAST(anime_categories AS BLOB))<=4096 AND json_valid(anime_categories) AND json_type(anime_categories)='array' AND json_array_length(anime_categories)<=64)),
    category TEXT CHECK (category IS NULL OR (length(trim(category))>0 AND length(CAST(category AS BLOB))<=64)),
    imported_category TEXT CHECK (imported_category IS NULL OR (length(trim(imported_category))>0 AND length(CAST(imported_category AS BLOB))<=64)),
    recent_priority INTEGER CHECK (recent_priority IS NULL OR (typeof(recent_priority)='integer' AND recent_priority BETWEEN -100 AND 2)),
    older_priority INTEGER CHECK (older_priority IS NULL OR (typeof(older_priority)='integer' AND older_priority BETWEEN -100 AND 2)), anime_standard_format_search INTEGER, remove_year INTEGER, initial_state TEXT, content_layout TEXT, sequential_order INTEGER, first_last_first INTEGER, add_tags INTEGER, enable_rss INTEGER NOT NULL DEFAULT 1 CHECK(typeof(enable_rss)='integer' AND enable_rss IN (0,1)), enable_automatic_search INTEGER NOT NULL DEFAULT 1 CHECK(typeof(enable_automatic_search)='integer' AND enable_automatic_search IN (0,1)), enable_interactive_search INTEGER NOT NULL DEFAULT 1 CHECK(typeof(enable_interactive_search)='integer' AND enable_interactive_search IN (0,1)), download_client_id TEXT DEFAULT NULL CHECK (
    download_client_id IS NULL OR (
        implementation IN ('torznab','newznab')
        AND typeof(download_client_id)='text'
        AND length(download_client_id)=36
        AND length(CAST(download_client_id AS BLOB))=36
        AND instr(download_client_id,char(0))=0
        AND substr(download_client_id,9,1)='-'
        AND substr(download_client_id,14,1)='-'
        AND substr(download_client_id,19,1)='-'
        AND substr(download_client_id,24,1)='-'
        AND length(replace(download_client_id,'-',''))=32
        AND replace(download_client_id,'-','') NOT GLOB '*[^0-9a-f]*'
    )
),
    PRIMARY KEY(provider_id,media_type),
    FOREIGN KEY(provider_id,implementation) REFERENCES providers(id,implementation) ON DELETE CASCADE
);
INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,category,imported_category,recent_priority,older_priority,anime_standard_format_search,remove_year,initial_state,content_layout,sequential_order,first_last_first,add_tags,enable_rss,enable_automatic_search,enable_interactive_search,download_client_id) SELECT provider_id,implementation,media_type,categories,anime_categories,category,imported_category,recent_priority,older_priority,anime_standard_format_search,remove_year,initial_state,content_layout,sequential_order,first_last_first,add_tags,enable_rss,enable_automatic_search,enable_interactive_search,download_client_id FROM _registry58_scopes;
DROP TABLE _registry58_scopes;
DROP TABLE _registry58_providers;

CREATE TRIGGER provider_scope_shape_insert BEFORE INSERT ON provider_scopes
WHEN NOT ((NEW.implementation IN ('torznab','newznab') AND NEW.categories IS NOT NULL AND NEW.anime_categories IS NOT NULL
    AND NEW.category IS NULL AND NEW.imported_category IS NULL AND NEW.recent_priority IS NULL AND NEW.older_priority IS NULL
    AND ((NEW.media_type='movies' AND json_array_length(NEW.categories)>0 AND json_array_length(NEW.anime_categories)=0)
      OR (NEW.media_type='tv' AND json_array_length(NEW.categories)+json_array_length(NEW.anime_categories)>0)))
  OR (NEW.implementation='qbittorrent' AND NEW.categories IS NULL AND NEW.anime_categories IS NULL AND NEW.category IS NOT NULL
    AND NEW.recent_priority IN (0,1) AND NEW.older_priority IN (0,1))
  OR (NEW.implementation='sabnzbd' AND NEW.categories IS NULL AND NEW.anime_categories IS NULL AND NEW.category IS NOT NULL
    AND NEW.imported_category IS NULL AND NEW.download_client_id IS NULL
    AND NEW.recent_priority IN (-100,-2,-1,0,1,2) AND NEW.older_priority IN (-100,-2,-1,0,1,2)))
BEGIN SELECT RAISE(ABORT,'invalid provider scope shape'); END;
CREATE TRIGGER provider_scope_shape_update BEFORE UPDATE ON provider_scopes
WHEN NOT ((NEW.implementation IN ('torznab','newznab') AND NEW.categories IS NOT NULL AND NEW.anime_categories IS NOT NULL
    AND NEW.category IS NULL AND NEW.imported_category IS NULL AND NEW.recent_priority IS NULL AND NEW.older_priority IS NULL
    AND ((NEW.media_type='movies' AND json_array_length(NEW.categories)>0 AND json_array_length(NEW.anime_categories)=0)
      OR (NEW.media_type='tv' AND json_array_length(NEW.categories)+json_array_length(NEW.anime_categories)>0)))
  OR (NEW.implementation='qbittorrent' AND NEW.categories IS NULL AND NEW.anime_categories IS NULL AND NEW.category IS NOT NULL
    AND NEW.recent_priority IN (0,1) AND NEW.older_priority IN (0,1))
  OR (NEW.implementation='sabnzbd' AND NEW.categories IS NULL AND NEW.anime_categories IS NULL AND NEW.category IS NOT NULL
    AND NEW.imported_category IS NULL AND NEW.download_client_id IS NULL
    AND NEW.recent_priority IN (-100,-2,-1,0,1,2) AND NEW.older_priority IN (-100,-2,-1,0,1,2)))
BEGIN SELECT RAISE(ABORT,'invalid provider scope shape'); END;
CREATE INDEX providers_order ON providers(priority,name,id);
CREATE TRIGGER provider_identity_immutable BEFORE UPDATE OF id,implementation,settings_version ON providers
WHEN NEW.id IS NOT OLD.id OR NEW.implementation IS NOT OLD.implementation OR NEW.settings_version IS NOT OLD.settings_version
BEGIN SELECT RAISE(ABORT,'provider identity is immutable'); END;
CREATE TRIGGER provider_revision_step BEFORE UPDATE ON providers
WHEN NEW.revision IS NOT OLD.revision+1
BEGIN SELECT RAISE(ABORT,'provider update requires the next revision'); END;
CREATE TRIGGER provider_configuration_invalidates_test AFTER UPDATE ON providers
BEGIN DELETE FROM provider_tests WHERE provider_id=OLD.id; END;
CREATE TRIGGER refresh_provider_changed AFTER UPDATE ON providers
BEGIN
 DELETE FROM download_refresh_snapshots WHERE provider_id=OLD.id;
 UPDATE download_refresh_schedules SET enabled=0,error_code='provider_changed',revision=min(revision+1,9007199254740991) WHERE provider_id=OLD.id;
END;
CREATE TRIGGER rss_provider_changed AFTER UPDATE ON providers BEGIN
 UPDATE rss_schedules SET enabled=0,error_code='provider_changed',revision=min(revision+1,9007199254740991) WHERE indexer_id=OLD.id OR client_id=OLD.id;
END;
CREATE TRIGGER processing_provider_changed AFTER UPDATE ON providers BEGIN
 UPDATE download_processing_policies SET enabled=0,revision=revision+1 WHERE provider_id=OLD.id;
END;
CREATE TRIGGER indexer_status_configuration_reset AFTER UPDATE OF revision ON providers
WHEN NEW.revision IS NOT OLD.revision
BEGIN DELETE FROM indexer_status WHERE provider_id=NEW.id; END;
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
CREATE TRIGGER provider_scope_insert_invalidates_test AFTER INSERT ON provider_scopes
BEGIN DELETE FROM provider_tests WHERE provider_id=NEW.provider_id; END;
CREATE TRIGGER provider_scope_delete_invalidates_test AFTER DELETE ON provider_scopes
BEGIN DELETE FROM provider_tests WHERE provider_id=OLD.provider_id; END;
CREATE TRIGGER provider_scope_update_invalidates_test AFTER UPDATE ON provider_scopes
BEGIN DELETE FROM provider_tests WHERE provider_id=OLD.provider_id OR provider_id=NEW.provider_id; END;
CREATE TRIGGER refresh_scope_insert AFTER INSERT ON provider_scopes
BEGIN
 DELETE FROM download_refresh_snapshots WHERE provider_id=NEW.provider_id AND media_type=NEW.media_type;
 UPDATE download_refresh_schedules SET enabled=0,error_code='provider_changed',revision=min(revision+1,9007199254740991) WHERE provider_id=NEW.provider_id AND media_type=NEW.media_type;
END;
CREATE TRIGGER refresh_scope_update AFTER UPDATE ON provider_scopes
BEGIN
 DELETE FROM download_refresh_snapshots WHERE (provider_id=OLD.provider_id AND media_type=OLD.media_type) OR (provider_id=NEW.provider_id AND media_type=NEW.media_type);
 UPDATE download_refresh_schedules SET enabled=0,error_code='provider_changed',revision=min(revision+1,9007199254740991) WHERE (provider_id=OLD.provider_id AND media_type=OLD.media_type) OR (provider_id=NEW.provider_id AND media_type=NEW.media_type);
END;
CREATE TRIGGER refresh_scope_delete AFTER DELETE ON provider_scopes
BEGIN
 UPDATE download_refresh_schedules SET enabled=0,error_code='provider_changed',revision=min(revision+1,9007199254740991) WHERE provider_id=OLD.provider_id AND media_type=OLD.media_type;
END;
CREATE TRIGGER rss_scope_insert AFTER INSERT ON provider_scopes BEGIN
 UPDATE rss_schedules SET enabled=0,error_code='provider_changed',revision=min(revision+1,9007199254740991) WHERE (indexer_id=NEW.provider_id OR client_id=NEW.provider_id) AND media_type=NEW.media_type;
END;
CREATE TRIGGER rss_scope_update AFTER UPDATE ON provider_scopes BEGIN
 UPDATE rss_schedules SET enabled=0,error_code='provider_changed',revision=min(revision+1,9007199254740991) WHERE (indexer_id=NEW.provider_id OR client_id=NEW.provider_id) AND media_type=NEW.media_type;
END;
CREATE TRIGGER rss_scope_delete AFTER DELETE ON provider_scopes BEGIN
 UPDATE rss_schedules SET enabled=0,error_code='provider_changed',revision=min(revision+1,9007199254740991) WHERE (indexer_id=OLD.provider_id OR client_id=OLD.provider_id) AND media_type=OLD.media_type;
END;
CREATE TRIGGER processing_scope_insert AFTER INSERT ON provider_scopes BEGIN
 UPDATE download_processing_policies SET enabled=0,revision=revision+1 WHERE provider_id=NEW.provider_id AND media_type=NEW.media_type;
END;
CREATE TRIGGER processing_scope_update AFTER UPDATE ON provider_scopes BEGIN
 UPDATE download_processing_policies SET enabled=0,revision=revision+1 WHERE (provider_id=OLD.provider_id AND media_type=OLD.media_type) OR (provider_id=NEW.provider_id AND media_type=NEW.media_type);
END;
CREATE TRIGGER processing_scope_delete AFTER DELETE ON provider_scopes BEGIN
 UPDATE download_processing_policies SET enabled=0,revision=revision+1 WHERE provider_id=OLD.provider_id AND media_type=OLD.media_type;
END;
-- SABnzbd category names are case-insensitive; TV and movies must never share one.
CREATE UNIQUE INDEX provider_usenet_categories ON provider_scopes(provider_id,lower(category)) WHERE implementation='sabnzbd';
CREATE TRIGGER provider_scope_options_insert BEFORE INSERT ON provider_scopes
WHEN NOT ((NEW.implementation IN ('torznab','newznab') AND NEW.media_type='tv'
    AND typeof(NEW.anime_standard_format_search)='integer' AND NEW.anime_standard_format_search IN (0,1) AND NEW.remove_year IS NULL)
 OR (NEW.implementation IN ('torznab','newznab') AND NEW.media_type='movies'
    AND typeof(NEW.remove_year)='integer' AND NEW.remove_year IN (0,1) AND NEW.anime_standard_format_search IS NULL)
 OR (NEW.implementation IN ('qbittorrent','sabnzbd') AND NEW.anime_standard_format_search IS NULL AND NEW.remove_year IS NULL))
BEGIN SELECT RAISE(ABORT,'invalid indexer scope options'); END;
CREATE TRIGGER provider_scope_options_update BEFORE UPDATE ON provider_scopes
WHEN NOT ((NEW.implementation IN ('torznab','newznab') AND NEW.media_type='tv'
    AND typeof(NEW.anime_standard_format_search)='integer' AND NEW.anime_standard_format_search IN (0,1) AND NEW.remove_year IS NULL)
 OR (NEW.implementation IN ('torznab','newznab') AND NEW.media_type='movies'
    AND typeof(NEW.remove_year)='integer' AND NEW.remove_year IN (0,1) AND NEW.anime_standard_format_search IS NULL)
 OR (NEW.implementation IN ('qbittorrent','sabnzbd') AND NEW.anime_standard_format_search IS NULL AND NEW.remove_year IS NULL))
BEGIN SELECT RAISE(ABORT,'invalid indexer scope options'); END;
CREATE TRIGGER provider_client_options_insert BEFORE INSERT ON provider_scopes
WHEN NOT ((NEW.implementation='qbittorrent'
 AND typeof(NEW.initial_state)='text' AND NEW.initial_state IN ('started','stopped','forced')
 AND typeof(NEW.content_layout)='text' AND NEW.content_layout IN ('default','original','subfolder')
 AND typeof(NEW.sequential_order)='integer' AND NEW.sequential_order IN (0,1)
 AND typeof(NEW.first_last_first)='integer' AND NEW.first_last_first IN (0,1)
 AND typeof(NEW.add_tags)='integer' AND NEW.add_tags IN (0,1) AND (NEW.media_type='tv' OR NEW.add_tags=0))
 OR (NEW.implementation IN ('torznab','newznab','sabnzbd') AND NEW.initial_state IS NULL AND NEW.content_layout IS NULL
 AND NEW.sequential_order IS NULL AND NEW.first_last_first IS NULL AND NEW.add_tags IS NULL))
BEGIN SELECT RAISE(ABORT,'invalid client scope options'); END;
CREATE TRIGGER provider_client_options_update BEFORE UPDATE ON provider_scopes
WHEN NOT ((NEW.implementation='qbittorrent'
 AND typeof(NEW.initial_state)='text' AND NEW.initial_state IN ('started','stopped','forced')
 AND typeof(NEW.content_layout)='text' AND NEW.content_layout IN ('default','original','subfolder')
 AND typeof(NEW.sequential_order)='integer' AND NEW.sequential_order IN (0,1)
 AND typeof(NEW.first_last_first)='integer' AND NEW.first_last_first IN (0,1)
 AND typeof(NEW.add_tags)='integer' AND NEW.add_tags IN (0,1) AND (NEW.media_type='tv' OR NEW.add_tags=0))
 OR (NEW.implementation IN ('torznab','newznab','sabnzbd') AND NEW.initial_state IS NULL AND NEW.content_layout IS NULL
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
CREATE VIEW cdh_scope_authority AS
 SELECT p.id provider_id,s.media_type,p.revision provider_revision,
        CASE WHEN p.enabled=1 AND d.enabled=1 AND coalesce(a.enabled_override,1)=1 THEN 1 ELSE 0 END desired_enabled
 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id
 JOIN completed_download_handling_settings d ON d.media_type=s.media_type
 LEFT JOIN download_processing_policies a ON a.provider_id=p.id AND a.media_type=s.media_type
 WHERE p.implementation IN (SELECT implementation FROM provider_implementations WHERE kind='download_client');
DROP TRIGGER provider_test_insert_owner;
CREATE TRIGGER provider_test_insert_owner BEFORE INSERT ON provider_tests
WHEN NOT EXISTS(SELECT 1 FROM providers WHERE id=NEW.provider_id AND revision=NEW.config_revision AND implementation IN (SELECT implementation FROM provider_implementations))
BEGIN SELECT RAISE(ABORT,'provider test revision or implementation mismatch'); END;
DROP TRIGGER provider_test_update_owner;
CREATE TRIGGER provider_test_update_owner BEFORE UPDATE ON provider_tests
WHEN NEW.provider_id IS NOT OLD.provider_id OR NOT EXISTS(SELECT 1 FROM providers WHERE id=NEW.provider_id AND revision=NEW.config_revision AND implementation IN (SELECT implementation FROM provider_implementations))
BEGIN SELECT RAISE(ABORT,'provider test revision or implementation mismatch'); END;
DROP TRIGGER refresh_schedule_admit;
CREATE TRIGGER refresh_schedule_admit BEFORE INSERT ON download_refresh_schedules BEGIN
 SELECT CASE WHEN NEW.intent!='suppressed' AND (SELECT count(*) FROM download_refresh_schedules WHERE intent!='suppressed')>=64 AND NOT EXISTS(SELECT 1 FROM download_refresh_schedules WHERE provider_id=NEW.provider_id AND media_type=NEW.media_type AND intent!='suppressed') THEN RAISE(ABORT,'refresh schedule capacity reached') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.provider_id AND p.revision=NEW.provider_revision AND p.implementation IN (SELECT implementation FROM provider_implementations WHERE kind='download_client') AND s.media_type=NEW.media_type AND (NEW.enabled=0 OR p.enabled=1)) THEN RAISE(ABORT,'invalid schedule provider scope or revision') END;
 SELECT CASE WHEN (NEW.intent='explicit')!=(NEW.requested_enabled IS NOT NULL) OR (NEW.intent='suppressed' AND NEW.enabled!=0) THEN RAISE(ABORT,'invalid refresh intent') END;
END;
DROP TRIGGER refresh_schedule_update;
CREATE TRIGGER refresh_schedule_update BEFORE UPDATE ON download_refresh_schedules
BEGIN
 SELECT CASE WHEN NEW.provider_id IS NOT OLD.provider_id OR NEW.media_type IS NOT OLD.media_type THEN RAISE(ABORT,'schedule identity is immutable') END;
 SELECT CASE WHEN (NEW.enabled=1 OR NEW.provider_revision IS NOT OLD.provider_revision) AND NOT EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.provider_id AND p.revision=NEW.provider_revision AND p.implementation IN (SELECT implementation FROM provider_implementations WHERE kind='download_client') AND s.media_type=NEW.media_type AND (NEW.enabled=0 OR p.enabled=1)) THEN RAISE(ABORT,'invalid schedule provider scope or revision') END;
END;
DROP TRIGGER refresh_snapshot_insert;
CREATE TRIGGER refresh_snapshot_insert BEFORE INSERT ON download_refresh_snapshots
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM download_refresh_snapshots)>=64 AND NOT EXISTS(SELECT 1 FROM download_refresh_snapshots WHERE provider_id=NEW.provider_id AND media_type=NEW.media_type) THEN RAISE(ABORT,'refresh snapshot capacity reached') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM providers WHERE id=NEW.provider_id AND revision=NEW.provider_revision AND enabled=1 AND implementation IN (SELECT implementation FROM provider_implementations WHERE kind='download_client')) THEN RAISE(ABORT,'stale refresh snapshot') END;
 SELECT CASE WHEN NEW.command_id IS NULL OR NOT EXISTS(SELECT 1 FROM commands WHERE id=NEW.command_id AND provider_id=NEW.provider_id AND media_type=NEW.media_type AND provider_revision=NEW.provider_revision AND status='succeeded' AND items_observed=json_array_length(NEW.items_json)) THEN RAISE(ABORT,'snapshot command mismatch') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM json_each(NEW.items_json) WHERE type!='object' OR json_extract(value,'$.domain') IS NOT NEW.media_type OR json_type(value,'$.hash') IS NOT 'text' OR length(json_extract(value,'$.hash')) NOT IN (40,64) OR json_extract(value,'$.hash') GLOB '*[^0-9a-f]*') OR (SELECT count(*) FROM json_each(NEW.items_json))!=(SELECT count(DISTINCT json_extract(value,'$.hash')) FROM json_each(NEW.items_json)) THEN RAISE(ABORT,'invalid snapshot domain or download identity') END;
END;
DROP TRIGGER refresh_snapshot_update;
CREATE TRIGGER refresh_snapshot_update BEFORE UPDATE ON download_refresh_snapshots
BEGIN
 SELECT CASE WHEN NEW.provider_id IS NOT OLD.provider_id OR NEW.media_type IS NOT OLD.media_type THEN RAISE(ABORT,'snapshot identity is immutable') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM providers WHERE id=NEW.provider_id AND revision=NEW.provider_revision AND enabled=1 AND implementation IN (SELECT implementation FROM provider_implementations WHERE kind='download_client')) THEN RAISE(ABORT,'stale refresh snapshot') END;
 SELECT CASE WHEN NEW.command_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM commands WHERE id=NEW.command_id AND provider_id=NEW.provider_id AND media_type=NEW.media_type AND provider_revision=NEW.provider_revision AND status='succeeded' AND items_observed=json_array_length(NEW.items_json)) THEN RAISE(ABORT,'snapshot command mismatch') END;
 SELECT CASE WHEN NEW.command_id IS NULL AND (NEW.provider_revision IS NOT OLD.provider_revision OR NEW.observed_at IS NOT OLD.observed_at OR NEW.items_json IS NOT OLD.items_json) THEN RAISE(ABORT,'snapshot replacement requires a command') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM json_each(NEW.items_json) WHERE type!='object' OR json_extract(value,'$.domain') IS NOT NEW.media_type OR json_type(value,'$.hash') IS NOT 'text' OR length(json_extract(value,'$.hash')) NOT IN (40,64) OR json_extract(value,'$.hash') GLOB '*[^0-9a-f]*') OR (SELECT count(*) FROM json_each(NEW.items_json))!=(SELECT count(DISTINCT json_extract(value,'$.hash')) FROM json_each(NEW.items_json)) THEN RAISE(ABORT,'invalid snapshot domain or download identity') END;
END;
DROP TRIGGER processing_policy_insert;
CREATE TRIGGER processing_policy_insert BEFORE INSERT ON download_processing_policies BEGIN
 SELECT CASE WHEN NEW.revision!=1 OR NOT EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.provider_id AND p.revision=NEW.provider_revision AND p.implementation IN (SELECT implementation FROM provider_implementations WHERE kind='download_client') AND s.media_type=NEW.media_type AND (NEW.enabled=0 OR p.enabled=1)) THEN RAISE(ABORT,'invalid processing policy') END;
END;
DROP TRIGGER processing_policy_update;
CREATE TRIGGER processing_policy_update BEFORE UPDATE ON download_processing_policies BEGIN
 SELECT CASE WHEN NEW.provider_id IS NOT OLD.provider_id OR NEW.media_type IS NOT OLD.media_type OR NEW.revision!=OLD.revision+1 THEN RAISE(ABORT,'invalid processing policy revision') END;
 SELECT CASE WHEN NEW.enabled=1 AND NOT EXISTS(SELECT 1 FROM completed_download_handling_settings WHERE media_type=NEW.media_type AND enabled=1) THEN RAISE(ABORT,'completed handling disabled') END;
 SELECT CASE WHEN (NEW.enabled=1 OR NEW.provider_revision IS NOT OLD.provider_revision) AND NOT EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.provider_id AND p.revision=NEW.provider_revision AND p.implementation IN (SELECT implementation FROM provider_implementations WHERE kind='download_client') AND s.media_type=NEW.media_type AND (NEW.enabled=0 OR p.enabled=1)) THEN RAISE(ABORT,'stale processing policy') END;
END;
DROP TRIGGER download_client_status_insert;
CREATE TRIGGER download_client_status_insert BEFORE INSERT ON download_client_status BEGIN
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.provider_id AND p.implementation IN (SELECT implementation FROM provider_implementations WHERE kind='download_client') AND p.revision=NEW.config_revision AND s.media_type=NEW.media_type) THEN RAISE(ABORT,'invalid download client status owner') END;
END;
DROP TRIGGER download_client_status_update;
CREATE TRIGGER download_client_status_update BEFORE UPDATE ON download_client_status BEGIN
 SELECT CASE WHEN NEW.provider_id IS NOT OLD.provider_id OR NEW.media_type IS NOT OLD.media_type THEN RAISE(ABORT,'download client status identity is immutable') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.provider_id AND p.implementation IN (SELECT implementation FROM provider_implementations WHERE kind='download_client') AND p.revision=NEW.config_revision AND s.media_type=NEW.media_type) THEN RAISE(ABORT,'invalid download client status owner') END;
END;
DROP TRIGGER commands_transition;
CREATE TRIGGER commands_transition BEFORE UPDATE ON commands
WHEN NOT (OLD.origin='automatic' AND OLD.status IN ('queued','running','retry_wait') AND OLD.manual_bypass=0 AND NEW.manual_bypass=1 AND NEW.id IS OLD.id AND NEW.name IS OLD.name AND NEW.provider_id IS OLD.provider_id AND NEW.media_type IS OLD.media_type AND NEW.provider_revision IS OLD.provider_revision AND NEW.priority IS OLD.priority AND NEW.status IS OLD.status AND NEW.attempts IS OLD.attempts AND NEW.next_attempt_at IS OLD.next_attempt_at AND NEW.created_at IS OLD.created_at AND NEW.started_at IS OLD.started_at AND NEW.completed_at IS OLD.completed_at AND NEW.error_code IS OLD.error_code AND NEW.items_observed IS OLD.items_observed AND NEW.origin IS OLD.origin)
BEGIN
 SELECT CASE WHEN NEW.id IS NOT OLD.id OR NEW.name IS NOT OLD.name OR NEW.provider_id IS NOT OLD.provider_id OR NEW.media_type IS NOT OLD.media_type OR NEW.provider_revision IS NOT OLD.provider_revision OR NEW.created_at IS NOT OLD.created_at OR NEW.priority IS NOT OLD.priority THEN RAISE(ABORT,'command identity is immutable') END;
 SELECT CASE WHEN NOT ((OLD.status IN ('queued','retry_wait') AND NEW.status IN ('running','cancelled','failed')) OR (OLD.status='running' AND NEW.status IN ('running','retry_wait','succeeded','failed','cancelled'))) THEN RAISE(ABORT,'invalid command transition') END;
 SELECT CASE WHEN NEW.attempts != OLD.attempts + CASE WHEN NEW.status='running' AND OLD.status IN ('queued','retry_wait') THEN 1 ELSE 0 END THEN RAISE(ABORT,'invalid command attempt counter') END;
 SELECT CASE WHEN NEW.status IN ('running','succeeded') AND NOT EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.provider_id AND p.revision=NEW.provider_revision AND p.enabled=1 AND p.implementation IN (SELECT implementation FROM provider_implementations WHERE kind='download_client') AND s.media_type=NEW.media_type) THEN RAISE(ABORT,'stale refresh command') END;
END;
DROP TRIGGER commands_admit;
CREATE TRIGGER commands_admit BEFORE INSERT ON commands
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.provider_id AND p.revision=NEW.provider_revision AND p.enabled=1 AND p.implementation IN (SELECT implementation FROM provider_implementations WHERE kind='download_client') AND s.media_type=NEW.media_type) THEN RAISE(ABORT,'invalid refresh provider scope or revision') END;
END;
DROP TRIGGER download_client_tags_admit;
CREATE TRIGGER download_client_tags_admit BEFORE INSERT ON download_client_tags BEGIN
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM provider_scopes WHERE provider_id=NEW.provider_id AND media_type=NEW.media_type AND implementation IN (SELECT implementation FROM provider_implementations WHERE kind='download_client')) THEN RAISE(ABORT,'download client tag requires scoped client') END;
 SELECT CASE WHEN (SELECT count(*) FROM download_client_tags WHERE provider_id=NEW.provider_id AND media_type=NEW.media_type)>=200 THEN RAISE(ABORT,'download client tag capacity reached') END;
END;
DROP TRIGGER processing_admit;
CREATE TRIGGER processing_admit BEFORE INSERT ON download_processing BEGIN
 SELECT CASE WHEN NEW.status!='queued' OR NEW.preflight_attempts!=0 OR NEW.total_preflight_attempts!=0 OR NEW.resume_requested!=0 THEN RAISE(ABORT,'processing must be queued') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM rss_candidates r JOIN download_processing_policies p ON p.provider_id=r.client_id AND p.media_type=r.media_type JOIN providers v ON v.id=p.provider_id JOIN provider_scopes s ON s.provider_id=v.id AND s.media_type=r.media_type WHERE r.id=NEW.candidate_id AND r.status='observed' AND p.enabled=1 AND v.enabled=1 AND v.implementation IN (SELECT implementation FROM provider_implementations WHERE kind='download_client') AND p.revision=NEW.policy_revision AND p.provider_revision=r.client_revision AND v.revision=r.client_revision) THEN RAISE(ABORT,'processing requires current authorized observed receipt') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM json_each(NEW.reasons_json) WHERE type!='text' OR length(value)>128 OR value GLOB '*[^a-z0-9_]*') THEN RAISE(ABORT,'invalid processing reason') END;
END;
DROP TRIGGER rss_schedules_admit;
CREATE TRIGGER rss_schedules_admit BEFORE INSERT ON rss_schedules BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM rss_schedules)>=64 THEN RAISE(ABORT,'RSS schedule capacity reached') END;
 SELECT CASE WHEN NOT (EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.indexer_id AND p.revision=NEW.indexer_revision AND p.implementation IN ('torznab','newznab') AND s.media_type=NEW.media_type AND (NEW.enabled=0 OR p.enabled=1)) AND (NEW.selection_kind='default' OR EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.client_id AND p.revision=NEW.client_revision AND p.implementation IN (SELECT c.implementation FROM provider_implementations c JOIN provider_implementations i ON i.protocol=c.protocol JOIN providers x ON x.implementation=i.implementation WHERE c.kind='download_client' AND x.id=NEW.indexer_id) AND s.media_type=NEW.media_type AND (NEW.enabled=0 OR p.enabled=1)))) THEN RAISE(ABORT,'invalid RSS schedule provider scope or revision') END;
END;
DROP TRIGGER rss_schedules_update;
CREATE TRIGGER rss_schedules_update BEFORE UPDATE ON rss_schedules BEGIN
 SELECT CASE WHEN NEW.selection_kind IS NOT OLD.selection_kind OR NEW.id IS NOT OLD.id OR NEW.media_type IS NOT OLD.media_type OR NEW.indexer_id IS NOT OLD.indexer_id OR NEW.client_id IS NOT OLD.client_id OR NEW.created_at IS NOT OLD.created_at THEN RAISE(ABORT,'RSS schedule identity is immutable') END;
 SELECT CASE WHEN (NEW.enabled=1 OR NEW.indexer_revision IS NOT OLD.indexer_revision OR NEW.client_revision IS NOT OLD.client_revision) AND NOT (EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.indexer_id AND p.revision=NEW.indexer_revision AND p.implementation IN ('torznab','newznab') AND s.media_type=NEW.media_type AND (NEW.enabled=0 OR p.enabled=1)) AND (NEW.selection_kind='default' OR EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.client_id AND p.revision=NEW.client_revision AND p.implementation IN (SELECT c.implementation FROM provider_implementations c JOIN provider_implementations i ON i.protocol=c.protocol JOIN providers x ON x.implementation=i.implementation WHERE c.kind='download_client' AND x.id=NEW.indexer_id) AND s.media_type=NEW.media_type AND (NEW.enabled=0 OR p.enabled=1)))) THEN RAISE(ABORT,'invalid RSS schedule provider scope or revision') END;
END;
DROP TRIGGER rss_commands_admit;
CREATE TRIGGER rss_commands_admit BEFORE INSERT ON rss_commands BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 OR NEW.fetch_complete!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT (EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.indexer_id AND p.revision=NEW.indexer_revision AND p.implementation IN ('torznab','newznab') AND s.media_type=NEW.media_type AND p.enabled=1) AND (NEW.selection_kind='default' OR EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.client_id AND p.revision=NEW.client_revision AND p.implementation IN (SELECT c.implementation FROM provider_implementations c JOIN provider_implementations i ON i.protocol=c.protocol JOIN providers x ON x.implementation=i.implementation WHERE c.kind='download_client' AND x.id=NEW.indexer_id) AND s.media_type=NEW.media_type AND p.enabled=1))) THEN RAISE(ABORT,'invalid RSS provider scope or revision') END;
END;
DROP TRIGGER search_commands_admit;
CREATE TRIGGER search_commands_admit BEFORE INSERT ON search_commands BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM metadata_refresh_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rss_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM search_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM manual_import_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM quality_reset_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rescan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))>=1024 THEN RAISE(ABORT,'command capacity reached') END;
 SELECT CASE WHEN NEW.status!='queued' OR NEW.attempts!=0 OR NEW.fetch_complete!=0 THEN RAISE(ABORT,'commands must be enqueued') END;
 SELECT CASE WHEN NOT (EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.indexer_id AND p.revision=NEW.indexer_revision AND p.implementation IN ('torznab','newznab') AND s.media_type=NEW.media_type AND p.enabled=1) AND (NEW.selection_kind='default' OR EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.client_id AND p.revision=NEW.client_revision AND p.implementation IN (SELECT c.implementation FROM provider_implementations c JOIN provider_implementations i ON i.protocol=c.protocol JOIN providers x ON x.implementation=i.implementation WHERE c.kind='download_client' AND x.id=NEW.indexer_id) AND s.media_type=NEW.media_type AND p.enabled=1))) THEN RAISE(ABORT,'invalid RSS provider scope or revision') END;
 SELECT CASE WHEN NOT ((NEW.media_type='tv' AND EXISTS(SELECT 1 FROM episodes e WHERE e.id=NEW.requested_episode_id AND json_extract(NEW.captured_target_json,'$.media_type')='tv' AND json_extract(NEW.captured_target_json,'$.episode_id')=e.id AND json_extract(NEW.captured_target_json,'$.series_id')=e.series_id)) OR (NEW.media_type='movies' AND EXISTS(SELECT 1 FROM movies m WHERE m.id=NEW.requested_movie_id AND json_extract(NEW.captured_target_json,'$.media_type')='movies' AND json_extract(NEW.captured_target_json,'$.movie_id')=m.id AND json_extract(NEW.captured_target_json,'$.metadata_id')=m.metadata_id))) THEN RAISE(ABORT,'invalid search target') END;
END;
