-- Failure history belongs to a provider/domain, not to replaceable configuration scopes.
-- Configuration writers reconcile final scopes and rebind revision in their transaction.
CREATE TABLE download_client_status (
 provider_id TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
 media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
 config_revision INTEGER NOT NULL CHECK(typeof(config_revision)='integer' AND config_revision BETWEEN 1 AND 9007199254740991),
 escalation_level INTEGER NOT NULL CHECK(typeof(escalation_level)='integer' AND escalation_level BETWEEN 0 AND 5),
 initial_failure_at INTEGER CHECK(initial_failure_at IS NULL OR (typeof(initial_failure_at)='integer' AND initial_failure_at BETWEEN 0 AND 9007199254740991)),
 last_failure_at INTEGER CHECK(last_failure_at IS NULL OR (typeof(last_failure_at)='integer' AND last_failure_at BETWEEN 0 AND 9007199254740991)),
 disabled_until INTEGER CHECK(disabled_until IS NULL OR (typeof(disabled_until)='integer' AND disabled_until BETWEEN 0 AND 9007199254740991)),
 PRIMARY KEY(provider_id,media_type),
 CHECK((initial_failure_at IS NULL AND last_failure_at IS NULL AND escalation_level=0) OR (initial_failure_at IS NOT NULL AND last_failure_at IS NOT NULL AND last_failure_at>=initial_failure_at)),
 CHECK(disabled_until IS NULL OR (escalation_level>0 AND initial_failure_at IS NOT NULL))
);
CREATE TRIGGER download_client_status_insert BEFORE INSERT ON download_client_status BEGIN
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.provider_id AND p.implementation='qbittorrent' AND p.revision=NEW.config_revision AND s.media_type=NEW.media_type) THEN RAISE(ABORT,'invalid download client status owner') END;
END;
CREATE TRIGGER download_client_status_update BEFORE UPDATE ON download_client_status BEGIN
 SELECT CASE WHEN NEW.provider_id IS NOT OLD.provider_id OR NEW.media_type IS NOT OLD.media_type THEN RAISE(ABORT,'download client status identity is immutable') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.provider_id AND p.implementation='qbittorrent' AND p.revision=NEW.config_revision AND s.media_type=NEW.media_type) THEN RAISE(ABORT,'invalid download client status owner') END;
END;
-- Expiry is consumed independently of pending work and may be rearmed after publication.
ALTER TABLE health_checks ADD COLUMN next_expiry_at INTEGER CHECK(next_expiry_at IS NULL OR (typeof(next_expiry_at)='integer' AND next_expiry_at BETWEEN 0 AND 9007199254740991 AND scope IN ('tv','movies') AND check_key='download_client_backoff'));
INSERT INTO health_checks(scope,check_key,startup,scheduled,compatibility_type)VALUES
 ('tv','download_client_backoff',1,1,'DownloadClientStatusCheck'),
 ('movies','download_client_backoff',1,1,'DownloadClientStatusCheck');
-- Existing origin is unknown; priority is not evidence of manual or automatic admission.
ALTER TABLE commands ADD COLUMN origin TEXT NOT NULL DEFAULT 'legacy_unknown' CHECK(origin IN ('legacy_unknown','automatic','manual'));
ALTER TABLE commands ADD COLUMN manual_bypass INTEGER NOT NULL DEFAULT 0 CHECK(typeof(manual_bypass)='integer' AND manual_bypass IN (0,1));
DROP TRIGGER commands_transition;
CREATE TRIGGER commands_transition BEFORE UPDATE ON commands
WHEN NOT (OLD.origin='automatic' AND OLD.status IN ('queued','running','retry_wait') AND OLD.manual_bypass=0 AND NEW.manual_bypass=1 AND NEW.id IS OLD.id AND NEW.name IS OLD.name AND NEW.provider_id IS OLD.provider_id AND NEW.media_type IS OLD.media_type AND NEW.provider_revision IS OLD.provider_revision AND NEW.priority IS OLD.priority AND NEW.status IS OLD.status AND NEW.attempts IS OLD.attempts AND NEW.next_attempt_at IS OLD.next_attempt_at AND NEW.created_at IS OLD.created_at AND NEW.started_at IS OLD.started_at AND NEW.completed_at IS OLD.completed_at AND NEW.error_code IS OLD.error_code AND NEW.items_observed IS OLD.items_observed AND NEW.origin IS OLD.origin)
BEGIN
 SELECT CASE WHEN NEW.id IS NOT OLD.id OR NEW.name IS NOT OLD.name OR NEW.provider_id IS NOT OLD.provider_id OR NEW.media_type IS NOT OLD.media_type OR NEW.provider_revision IS NOT OLD.provider_revision OR NEW.created_at IS NOT OLD.created_at OR NEW.priority IS NOT OLD.priority THEN RAISE(ABORT,'command identity is immutable') END;
 SELECT CASE WHEN NOT ((OLD.status IN ('queued','retry_wait') AND NEW.status IN ('running','cancelled','failed')) OR (OLD.status='running' AND NEW.status IN ('running','retry_wait','succeeded','failed','cancelled'))) THEN RAISE(ABORT,'invalid command transition') END;
 SELECT CASE WHEN NEW.attempts != OLD.attempts + CASE WHEN NEW.status='running' AND OLD.status IN ('queued','retry_wait') THEN 1 ELSE 0 END THEN RAISE(ABORT,'invalid command attempt counter') END;
 SELECT CASE WHEN NEW.status IN ('running','succeeded') AND NOT EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.provider_id AND p.revision=NEW.provider_revision AND p.enabled=1 AND p.implementation='qbittorrent' AND s.media_type=NEW.media_type) THEN RAISE(ABORT,'stale refresh command') END;
END;
CREATE TRIGGER commands_backoff_identity BEFORE UPDATE ON commands BEGIN
 SELECT CASE WHEN NEW.origin IS NOT OLD.origin THEN RAISE(ABORT,'command origin is immutable') END;
 SELECT CASE WHEN NEW.manual_bypass IS NOT OLD.manual_bypass AND NOT (OLD.origin='automatic' AND OLD.status IN ('queued','running','retry_wait') AND OLD.manual_bypass=0 AND NEW.manual_bypass=1 AND NEW.id IS OLD.id AND NEW.name IS OLD.name AND NEW.provider_id IS OLD.provider_id AND NEW.media_type IS OLD.media_type AND NEW.provider_revision IS OLD.provider_revision AND NEW.priority IS OLD.priority AND NEW.status IS OLD.status AND NEW.attempts IS OLD.attempts AND NEW.next_attempt_at IS OLD.next_attempt_at AND NEW.created_at IS OLD.created_at AND NEW.started_at IS OLD.started_at AND NEW.completed_at IS OLD.completed_at AND NEW.error_code IS OLD.error_code AND NEW.items_observed IS OLD.items_observed AND NEW.origin IS OLD.origin) THEN RAISE(ABORT,'invalid manual bypass promotion') END;
END;
