-- Removed source metadata is a library-membership diagnostic, not deletion permission.
-- Start unobserved; the accepted startup/manual/scheduled lifecycle owns evaluation.
INSERT INTO health_checks(scope,check_key,startup,scheduled,compatibility_type)VALUES
 ('tv','removed_metadata',1,1,'RemovedSeriesCheck'),
 ('movies','removed_metadata',1,1,'RemovedMovieCheck');
-- Invalidate in the source mutation transaction, including snapshot/adoption writers.
-- Preserve old observations and all pending reasons; never postpone earlier due work.
CREATE TRIGGER removed_metadata_series_insert AFTER INSERT ON series
WHEN NEW.status='deleted'
BEGIN
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM health_checks WHERE scope='tv' AND check_key='removed_metadata')
  THEN RAISE(ABORT,'removed metadata health registry is missing') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM health_checks WHERE scope='tv' AND check_key='removed_metadata' AND generation>=9007199254740991)
  THEN RAISE(ABORT,'removed metadata health generation exhausted') END;
 UPDATE health_checks SET generation=generation+1,pending_reasons=pending_reasons|8,
  due_at=CASE WHEN due_at IS NULL THEN unixepoch()+5 ELSE min(due_at,unixepoch()+5) END
  WHERE scope='tv' AND check_key='removed_metadata';
END;
CREATE TRIGGER removed_metadata_series_delete AFTER DELETE ON series
WHEN OLD.status='deleted'
BEGIN
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM health_checks WHERE scope='tv' AND check_key='removed_metadata')
  THEN RAISE(ABORT,'removed metadata health registry is missing') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM health_checks WHERE scope='tv' AND check_key='removed_metadata' AND generation>=9007199254740991)
  THEN RAISE(ABORT,'removed metadata health generation exhausted') END;
 UPDATE health_checks SET generation=generation+1,pending_reasons=pending_reasons|8,
  due_at=CASE WHEN due_at IS NULL THEN unixepoch()+5 ELSE min(due_at,unixepoch()+5) END
  WHERE scope='tv' AND check_key='removed_metadata';
END;
CREATE TRIGGER removed_metadata_series_update AFTER UPDATE OF id,status,title,tvdb_id ON series
WHEN (OLD.status='deleted' OR NEW.status='deleted') AND (OLD.id IS NOT NEW.id OR OLD.status IS NOT NEW.status OR OLD.title IS NOT NEW.title OR OLD.tvdb_id IS NOT NEW.tvdb_id)
BEGIN
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM health_checks WHERE scope='tv' AND check_key='removed_metadata')
  THEN RAISE(ABORT,'removed metadata health registry is missing') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM health_checks WHERE scope='tv' AND check_key='removed_metadata' AND generation>=9007199254740991)
  THEN RAISE(ABORT,'removed metadata health generation exhausted') END;
 UPDATE health_checks SET generation=generation+1,pending_reasons=pending_reasons|8,
  due_at=CASE WHEN due_at IS NULL THEN unixepoch()+5 ELSE min(due_at,unixepoch()+5) END
  WHERE scope='tv' AND check_key='removed_metadata';
END;
CREATE TRIGGER removed_metadata_movie_insert AFTER INSERT ON movies
WHEN EXISTS(SELECT 1 FROM movie_metadata WHERE id=NEW.metadata_id AND status='deleted')
BEGIN
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM health_checks WHERE scope='movies' AND check_key='removed_metadata')
  THEN RAISE(ABORT,'removed metadata health registry is missing') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM health_checks WHERE scope='movies' AND check_key='removed_metadata' AND generation>=9007199254740991)
  THEN RAISE(ABORT,'removed metadata health generation exhausted') END;
 UPDATE health_checks SET generation=generation+1,pending_reasons=pending_reasons|8,
  due_at=CASE WHEN due_at IS NULL THEN unixepoch()+5 ELSE min(due_at,unixepoch()+5) END
  WHERE scope='movies' AND check_key='removed_metadata';
END;
CREATE TRIGGER removed_metadata_movie_delete AFTER DELETE ON movies
WHEN EXISTS(SELECT 1 FROM movie_metadata WHERE id=OLD.metadata_id AND status='deleted')
BEGIN
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM health_checks WHERE scope='movies' AND check_key='removed_metadata')
  THEN RAISE(ABORT,'removed metadata health registry is missing') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM health_checks WHERE scope='movies' AND check_key='removed_metadata' AND generation>=9007199254740991)
  THEN RAISE(ABORT,'removed metadata health generation exhausted') END;
 UPDATE health_checks SET generation=generation+1,pending_reasons=pending_reasons|8,
  due_at=CASE WHEN due_at IS NULL THEN unixepoch()+5 ELSE min(due_at,unixepoch()+5) END
  WHERE scope='movies' AND check_key='removed_metadata';
END;
CREATE TRIGGER removed_metadata_movie_update AFTER UPDATE OF id,metadata_id ON movies
WHEN (OLD.id IS NOT NEW.id OR OLD.metadata_id IS NOT NEW.metadata_id) AND EXISTS(SELECT 1 FROM movie_metadata WHERE id IN (OLD.metadata_id,NEW.metadata_id) AND status='deleted')
BEGIN
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM health_checks WHERE scope='movies' AND check_key='removed_metadata')
  THEN RAISE(ABORT,'removed metadata health registry is missing') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM health_checks WHERE scope='movies' AND check_key='removed_metadata' AND generation>=9007199254740991)
  THEN RAISE(ABORT,'removed metadata health generation exhausted') END;
 UPDATE health_checks SET generation=generation+1,pending_reasons=pending_reasons|8,
  due_at=CASE WHEN due_at IS NULL THEN unixepoch()+5 ELSE min(due_at,unixepoch()+5) END
  WHERE scope='movies' AND check_key='removed_metadata';
END;
CREATE TRIGGER removed_metadata_catalog_update AFTER UPDATE OF id,status,title,tmdb_id ON movie_metadata
WHEN (OLD.status='deleted' OR NEW.status='deleted') AND (OLD.id IS NOT NEW.id OR OLD.status IS NOT NEW.status OR OLD.title IS NOT NEW.title OR OLD.tmdb_id IS NOT NEW.tmdb_id) AND EXISTS(SELECT 1 FROM movies WHERE metadata_id IN (OLD.id,NEW.id))
BEGIN
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM health_checks WHERE scope='movies' AND check_key='removed_metadata')
  THEN RAISE(ABORT,'removed metadata health registry is missing') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM health_checks WHERE scope='movies' AND check_key='removed_metadata' AND generation>=9007199254740991)
  THEN RAISE(ABORT,'removed metadata health generation exhausted') END;
 UPDATE health_checks SET generation=generation+1,pending_reasons=pending_reasons|8,
  due_at=CASE WHEN due_at IS NULL THEN unixepoch()+5 ELSE min(due_at,unixepoch()+5) END
  WHERE scope='movies' AND check_key='removed_metadata';
END;
