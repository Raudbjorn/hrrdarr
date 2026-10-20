-- Observations belong to the captured configuration and one library domain.
CREATE TABLE indexer_status (
 provider_id TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
 media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
 config_revision INTEGER NOT NULL CHECK(typeof(config_revision)='integer' AND config_revision BETWEEN 1 AND 9007199254740991),
 failure_count INTEGER NOT NULL CHECK(typeof(failure_count)='integer' AND failure_count BETWEEN 1 AND 9007199254740991),
 initial_failure_at INTEGER NOT NULL CHECK(typeof(initial_failure_at)='integer' AND initial_failure_at BETWEEN 0 AND 9007199254740991),
 last_failure_at INTEGER NOT NULL CHECK(typeof(last_failure_at)='integer' AND last_failure_at BETWEEN initial_failure_at AND 9007199254740991),
 disabled_until INTEGER NOT NULL CHECK(typeof(disabled_until)='integer' AND disabled_until BETWEEN last_failure_at AND 9007199254740991),
 next_health_at INTEGER CHECK(next_health_at IS NULL OR (typeof(next_health_at)='integer' AND next_health_at BETWEEN last_failure_at AND disabled_until)),
 PRIMARY KEY(provider_id,media_type)
);
CREATE INDEX indexer_status_health_due ON indexer_status(next_health_at) WHERE next_health_at IS NOT NULL;
CREATE TRIGGER indexer_status_insert BEFORE INSERT ON indexer_status BEGIN
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.provider_id AND p.implementation IN ('torznab','newznab') AND p.revision=NEW.config_revision AND s.media_type=NEW.media_type) THEN RAISE(ABORT,'invalid indexer status owner') END;
END;
CREATE TRIGGER indexer_status_update BEFORE UPDATE ON indexer_status BEGIN
 SELECT CASE WHEN NEW.provider_id IS NOT OLD.provider_id OR NEW.media_type IS NOT OLD.media_type THEN RAISE(ABORT,'indexer status identity is immutable') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.provider_id AND p.implementation IN ('torznab','newznab') AND p.revision=NEW.config_revision AND s.media_type=NEW.media_type) THEN RAISE(ABORT,'invalid indexer status owner') END;
END;
-- Saving a configuration starts fresh; old in-flight observations retain the old revision.
CREATE TRIGGER indexer_status_configuration_reset AFTER UPDATE OF revision ON providers
WHEN NEW.revision IS NOT OLD.revision
BEGIN DELETE FROM indexer_status WHERE provider_id=NEW.id; END;
INSERT INTO health_checks(scope,check_key,startup,scheduled,compatibility_type)VALUES
 ('tv','indexer_status',1,1,'IndexerStatusCheck'),
 ('movies','indexer_status',1,1,'IndexerStatusCheck'),
 ('tv','indexer_failures',1,1,'IndexerLongTermStatusCheck'),
 ('movies','indexer_failures',1,1,'IndexerLongTermStatusCheck');
