-- The published download-client backoff migration is restored verbatim at its published version,
-- so the health deadline this branch's engine reads is added here instead of inside it. The
-- deadline is consumed once; expired failure history is retained.
ALTER TABLE download_client_status ADD COLUMN next_health_at INTEGER CHECK(next_health_at IS NULL OR (typeof(next_health_at)='integer' AND next_health_at BETWEEN 0 AND 9007199254740991 AND disabled_until IS NOT NULL AND next_health_at<=disabled_until));
CREATE INDEX download_client_status_health_due ON download_client_status(next_health_at) WHERE next_health_at IS NOT NULL;
