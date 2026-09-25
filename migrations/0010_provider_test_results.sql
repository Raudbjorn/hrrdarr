-- Only concrete indexer test results. Credentials, URLs, response bodies and capabilities
-- are deliberately absent. Replacing configuration invalidates the previous observation.
CREATE TABLE provider_tests (
    provider_id TEXT PRIMARY KEY NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
    config_revision INTEGER NOT NULL CHECK (typeof(config_revision)='integer' AND config_revision BETWEEN 1 AND 9007199254740991),
    tested_at INTEGER NOT NULL CHECK (typeof(tested_at)='integer' AND tested_at>=0),
    status TEXT NOT NULL CHECK (status IN ('success','failure')),
    error_code TEXT,
    CHECK ((status='success' AND error_code IS NULL) OR (status='failure' AND error_code IS NOT NULL AND error_code IN
      ('invalid_request','unsupported','authentication','rate_limited','invalid_response','transport_error','timeout','response_too_large','redirect_rejected')))
);
CREATE TRIGGER provider_test_insert_owner BEFORE INSERT ON provider_tests
WHEN NOT EXISTS(SELECT 1 FROM providers WHERE id=NEW.provider_id AND revision=NEW.config_revision AND implementation IN ('torznab','newznab'))
BEGIN SELECT RAISE(ABORT,'provider test revision or implementation mismatch'); END;
CREATE TRIGGER provider_test_update_owner BEFORE UPDATE ON provider_tests
WHEN NEW.provider_id IS NOT OLD.provider_id OR NOT EXISTS(SELECT 1 FROM providers WHERE id=NEW.provider_id AND revision=NEW.config_revision AND implementation IN ('torznab','newznab'))
BEGIN SELECT RAISE(ABORT,'provider test revision or implementation mismatch'); END;
CREATE TRIGGER provider_configuration_invalidates_test AFTER UPDATE ON providers
BEGIN DELETE FROM provider_tests WHERE provider_id=OLD.id; END;
-- Runtime scope replacements are in the same transaction as a provider revision update.
-- These guards also remove existing observations on direct scope maintenance; editing scopes
-- without advancing the revision during a live test is not a supported runtime write path.
CREATE TRIGGER provider_scope_insert_invalidates_test AFTER INSERT ON provider_scopes
BEGIN DELETE FROM provider_tests WHERE provider_id=NEW.provider_id; END;
CREATE TRIGGER provider_scope_update_invalidates_test AFTER UPDATE ON provider_scopes
BEGIN DELETE FROM provider_tests WHERE provider_id=OLD.provider_id OR provider_id=NEW.provider_id; END;
CREATE TRIGGER provider_scope_delete_invalidates_test AFTER DELETE ON provider_scopes
BEGIN DELETE FROM provider_tests WHERE provider_id=OLD.provider_id; END;
