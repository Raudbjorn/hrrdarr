-- Configuration only: existing file and immutable receipt revision facts remain intact.
CREATE TABLE revision_policies (
 media_type TEXT PRIMARY KEY NOT NULL CHECK(media_type IN ('tv','movies')),
 mode TEXT NOT NULL CHECK(mode IN ('prefer_and_upgrade','do_not_upgrade','do_not_prefer')),
 revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision BETWEEN 1 AND 9007199254740991),
 locally_edited INTEGER NOT NULL CHECK(locally_edited IN (0,1))
);
INSERT INTO revision_policies VALUES('tv','prefer_and_upgrade',1,0),('movies','prefer_and_upgrade',1,0);
ALTER TABLE snapshot_imports ADD COLUMN revision_policy_version INTEGER NOT NULL DEFAULT 0 CHECK(revision_policy_version IN (0,1));

-- Policy edits may wake a waiting parent without claiming it or changing any other field.
DROP TRIGGER rss_commands_transition;
CREATE TRIGGER rss_commands_transition BEFORE UPDATE ON rss_commands BEGIN
 SELECT CASE WHEN NEW.id IS NOT OLD.id OR NEW.name IS NOT OLD.name OR NEW.media_type IS NOT OLD.media_type OR NEW.indexer_id IS NOT OLD.indexer_id OR NEW.indexer_revision IS NOT OLD.indexer_revision OR NEW.client_id IS NOT OLD.client_id OR NEW.client_revision IS NOT OLD.client_revision OR NEW.priority IS NOT OLD.priority OR NEW.created_at IS NOT OLD.created_at THEN RAISE(ABORT,'command identity is immutable') END;
 SELECT CASE WHEN NOT ((OLD.status IN ('queued','retry_wait') AND NEW.next_attempt_at=0 AND OLD.next_attempt_at>0 AND NEW.id IS OLD.id AND NEW.name IS OLD.name AND NEW.media_type IS OLD.media_type AND NEW.priority IS OLD.priority AND NEW.status IS OLD.status AND NEW.attempts IS OLD.attempts AND NEW.created_at IS OLD.created_at AND NEW.started_at IS OLD.started_at AND NEW.completed_at IS OLD.completed_at AND NEW.indexer_id IS OLD.indexer_id AND NEW.indexer_revision IS OLD.indexer_revision AND NEW.client_id IS OLD.client_id AND NEW.client_revision IS OLD.client_revision AND NEW.error_code IS OLD.error_code AND NEW.fetched IS OLD.fetched AND NEW.evaluated IS OLD.evaluated AND NEW.rejected IS OLD.rejected AND NEW.pending IS OLD.pending AND NEW.observed IS OLD.observed AND NEW.uncertain IS OLD.uncertain AND NEW.fetch_complete IS OLD.fetch_complete) OR (OLD.status IN ('queued','retry_wait') AND NEW.status IN ('running','cancelled','failed')) OR (OLD.status='running' AND NEW.status IN ('running','retry_wait','succeeded','failed','cancelled'))) THEN RAISE(ABORT,'invalid command transition') END;
 SELECT CASE WHEN NEW.attempts != OLD.attempts + CASE WHEN NEW.status='running' AND OLD.status IN ('queued','retry_wait') THEN 1 ELSE 0 END THEN RAISE(ABORT,'invalid command attempt counter') END;
 SELECT CASE WHEN NEW.fetch_complete<OLD.fetch_complete OR (NEW.fetch_complete>OLD.fetch_complete AND NEW.status!='running') THEN RAISE(ABORT,'invalid RSS fetch checkpoint') END;
END;
