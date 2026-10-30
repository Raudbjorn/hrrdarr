-- Keep the published intent authority and its local-retention guards. The runner snapshots
-- rows before the historical supersede inside this same transaction. Already-superseded
-- databases have no rows to recover here; recovery of lost intents requires an older backup.
CREATE TABLE movie_collection_intents (
 tmdb_id INTEGER PRIMARY KEY CHECK(typeof(tmdb_id)='integer' AND tmdb_id BETWEEN 1 AND 9007199254740991),
 revision INTEGER NOT NULL DEFAULT 1 CHECK(typeof(revision)='integer' AND revision BETWEEN 1 AND 9007199254740991),
 local_edit INTEGER NOT NULL DEFAULT 0 CHECK(local_edit IN(0,1)),
 removed INTEGER NOT NULL DEFAULT 0 CHECK(removed IN(0,1)),
 removal_reason TEXT CHECK(removal_reason IN('user','metadata_missing')),
 CHECK((removed=0 AND removal_reason IS NULL) OR (removed=1 AND removal_reason IS NOT NULL)),
 CHECK(removal_reason IS NOT 'user' OR local_edit=1)
);
CREATE TRIGGER movie_collection_intents_revision BEFORE UPDATE ON movie_collection_intents BEGIN
 SELECT CASE WHEN NEW.tmdb_id IS NOT OLD.tmdb_id OR (OLD.local_edit=1 AND NEW.local_edit=0) OR OLD.revision=9007199254740991 OR NEW.revision!=OLD.revision+1 THEN RAISE(ABORT,'collection_intent_revision') END;
END;
CREATE TRIGGER movie_collection_intents_retain BEFORE DELETE ON movie_collection_intents WHEN OLD.local_edit=1 BEGIN
 SELECT RAISE(ABORT,'collection_local_intent_retained');
END;
CREATE TEMP TABLE IF NOT EXISTS retained_collection_intents AS
SELECT * FROM movie_collection_intents WHERE 0;
INSERT INTO movie_collection_intents
SELECT * FROM retained_collection_intents;
DROP TABLE temp.retained_collection_intents;
