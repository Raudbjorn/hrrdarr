-- Search-on-add is an outbox committed with library provenance. Historical intent is retained.
CREATE TABLE import_list_search_requests (
 addition_id INTEGER PRIMARY KEY REFERENCES import_list_additions(id) ON DELETE CASCADE,
 status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','admitted')),
 next_attempt_at INTEGER NOT NULL CHECK(typeof(next_attempt_at)='integer' AND next_attempt_at>=0),
 error_code TEXT CHECK(error_code IS NULL OR length(error_code) BETWEEN 1 AND 64),
 created_at INTEGER NOT NULL CHECK(typeof(created_at)='integer'),
 admitted_at INTEGER,
 CHECK((status='pending' AND admitted_at IS NULL) OR (status='admitted' AND admitted_at IS NOT NULL AND error_code IS NULL))
);
CREATE INDEX import_list_search_requests_ready ON import_list_search_requests(status,next_attempt_at,addition_id);
INSERT INTO import_list_search_requests(addition_id,next_attempt_at,created_at)
 SELECT id,0,added_at FROM import_list_additions WHERE search_requested=1;

-- A background command must never acquire user-search exceptions when a list or library item
-- disappears. Its authority marker therefore lives for the command's entire lifetime.
CREATE TABLE import_list_search_commands (
 command_id TEXT PRIMARY KEY REFERENCES search_commands(id) ON DELETE CASCADE,
 addition_id INTEGER REFERENCES import_list_additions(id) ON DELETE SET NULL
);
CREATE INDEX import_list_search_commands_addition ON import_list_search_commands(addition_id);
CREATE TRIGGER import_list_search_commands_insert BEFORE INSERT ON import_list_search_commands BEGIN
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM search_commands WHERE id=NEW.command_id AND mode='automatic' AND status='queued')
  OR NOT EXISTS(SELECT 1 FROM import_list_search_requests WHERE addition_id=NEW.addition_id AND status='pending')
 OR NOT EXISTS(SELECT 1 FROM search_commands c JOIN import_list_additions a ON a.id=NEW.addition_id
  WHERE c.id=NEW.command_id AND c.media_type=a.media_type
   AND ((a.media_type='movies' AND c.requested_movie_id=a.movie_id)
    OR (a.media_type='tv' AND EXISTS(SELECT 1 FROM episodes e WHERE e.id=c.requested_episode_id AND e.series_id=a.series_id))))
  THEN RAISE(ABORT,'background search requires pending import-list admission') END;
END;
CREATE TRIGGER import_list_search_commands_update BEFORE UPDATE ON import_list_search_commands
 WHEN NEW.command_id IS NOT OLD.command_id OR (NEW.addition_id IS NOT OLD.addition_id AND NEW.addition_id IS NOT NULL)
 BEGIN SELECT RAISE(ABORT,'background search authority is immutable'); END;
CREATE TRIGGER import_list_search_commands_delete BEFORE DELETE ON import_list_search_commands
 WHEN EXISTS(SELECT 1 FROM search_commands WHERE id=OLD.command_id)
 BEGIN SELECT RAISE(ABORT,'background search authority lasts until command deletion'); END;
