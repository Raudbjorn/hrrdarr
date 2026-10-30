-- Native last-search facts for Wanted (oldest-searched-first sort and search-all order).
-- Unix seconds written only when a search command's indexer dispatch phase ends after at least
-- one eligible indexer request was issued (including a caught indexer failure), before any
-- release decision or grab. Admission, cancellation before dispatch, RSS and grabs never write.
-- Imported snapshot values stay in episodes.last_search_time (TEXT) as imported facts; Radarr
-- snapshots carry no imported movie value here. Existing rows start NULL (never searched natively).
ALTER TABLE episodes ADD COLUMN native_last_search_at INTEGER CHECK(native_last_search_at IS NULL OR (typeof(native_last_search_at)='integer' AND native_last_search_at BETWEEN 0 AND 9007199254740991));
ALTER TABLE movies ADD COLUMN native_last_search_at INTEGER CHECK(native_last_search_at IS NULL OR (typeof(native_last_search_at)='integer' AND native_last_search_at BETWEEN 0 AND 9007199254740991));
-- Command progress: when this command's dispatch phase last completed (any attempt).
ALTER TABLE search_commands ADD COLUMN dispatch_completed_at INTEGER CHECK(dispatch_completed_at IS NULL OR (typeof(dispatch_completed_at)='integer' AND dispatch_completed_at BETWEEN 0 AND 9007199254740991));
CREATE TRIGGER search_commands_dispatch_admit BEFORE INSERT ON search_commands
WHEN NEW.dispatch_completed_at IS NOT NULL
BEGIN SELECT RAISE(ABORT,'search dispatch is recorded only after admission'); END;
CREATE TRIGGER search_commands_dispatch BEFORE UPDATE OF dispatch_completed_at ON search_commands
WHEN NEW.dispatch_completed_at IS NOT OLD.dispatch_completed_at
BEGIN
 SELECT CASE WHEN OLD.status!='running' OR NEW.status!='running' OR NEW.dispatch_completed_at IS NULL OR NEW.dispatch_completed_at<coalesce(OLD.dispatch_completed_at,0) THEN RAISE(ABORT,'search dispatch is recorded only while running and never moves backwards') END;
END;
