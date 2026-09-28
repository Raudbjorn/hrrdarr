-- Global intent is separate from concrete revision-fenced processing authority.
CREATE TABLE completed_download_handling_settings (
 media_type TEXT PRIMARY KEY NOT NULL CHECK(media_type IN ('tv','movies')),
 enabled INTEGER NOT NULL CHECK(typeof(enabled)='integer' AND enabled IN (0,1)),
 defined INTEGER NOT NULL CHECK(defined IN (0,1)),
 revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision BETWEEN 1 AND 9007199254740991),
 locally_edited INTEGER NOT NULL CHECK(locally_edited IN (0,1)),
 reconciliation_pending INTEGER NOT NULL CHECK(reconciliation_pending IN (0,1))
);
INSERT INTO completed_download_handling_settings VALUES('tv',1,0,1,0,1),('movies',1,0,1,0,1);
ALTER TABLE snapshot_imports ADD COLUMN cdh_version INTEGER NOT NULL DEFAULT 0 CHECK(cdh_version IN (0,1));
ALTER TABLE download_processing_policies ADD COLUMN enabled_override INTEGER CHECK(enabled_override IS NULL OR (typeof(enabled_override)='integer' AND enabled_override IN (0,1)));
-- False may be user intent or earlier safety invalidation: never infer permission from it.
DROP TRIGGER processing_policy_update;
UPDATE download_processing_policies SET enabled_override=enabled;
CREATE TRIGGER processing_policy_update BEFORE UPDATE ON download_processing_policies BEGIN
 SELECT CASE WHEN NEW.provider_id IS NOT OLD.provider_id OR NEW.media_type IS NOT OLD.media_type OR NEW.revision!=OLD.revision+1 THEN RAISE(ABORT,'invalid processing policy revision') END;
 SELECT CASE WHEN NEW.enabled=1 AND NOT EXISTS(SELECT 1 FROM completed_download_handling_settings WHERE media_type=NEW.media_type AND enabled=1) THEN RAISE(ABORT,'completed handling disabled') END;
 SELECT CASE WHEN (NEW.enabled=1 OR NEW.provider_revision IS NOT OLD.provider_revision) AND NOT EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.provider_id AND p.revision=NEW.provider_revision AND p.implementation='qbittorrent' AND s.media_type=NEW.media_type AND (NEW.enabled=0 OR p.enabled=1)) THEN RAISE(ABORT,'stale processing policy') END;
END;

CREATE TRIGGER processing_policy_master_insert BEFORE INSERT ON download_processing_policies WHEN NEW.enabled=1 BEGIN
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM completed_download_handling_settings WHERE media_type=NEW.media_type AND enabled=1) THEN RAISE(ABORT,'completed handling disabled') END;
END;
ALTER TABLE download_refresh_schedules ADD COLUMN intent TEXT NOT NULL DEFAULT 'explicit' CHECK(intent IN ('inherited','explicit','suppressed'));
ALTER TABLE download_refresh_schedules ADD COLUMN requested_enabled INTEGER CHECK(requested_enabled IS NULL OR (typeof(requested_enabled)='integer' AND requested_enabled IN (0,1)));
UPDATE download_refresh_schedules SET requested_enabled=enabled;
DROP TRIGGER refresh_schedule_admit;
CREATE TRIGGER refresh_schedule_admit BEFORE INSERT ON download_refresh_schedules BEGIN
 SELECT CASE WHEN NEW.intent!='suppressed' AND (SELECT count(*) FROM download_refresh_schedules WHERE intent!='suppressed')>=64 AND NOT EXISTS(SELECT 1 FROM download_refresh_schedules WHERE provider_id=NEW.provider_id AND media_type=NEW.media_type AND intent!='suppressed') THEN RAISE(ABORT,'refresh schedule capacity reached') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=NEW.provider_id AND p.revision=NEW.provider_revision AND p.implementation='qbittorrent' AND s.media_type=NEW.media_type AND (NEW.enabled=0 OR p.enabled=1)) THEN RAISE(ABORT,'invalid schedule provider scope or revision') END;
 SELECT CASE WHEN (NEW.intent='explicit')!=(NEW.requested_enabled IS NOT NULL) OR (NEW.intent='suppressed' AND NEW.enabled!=0) THEN RAISE(ABORT,'invalid refresh intent') END;
END;
CREATE TRIGGER refresh_schedule_intent_update BEFORE UPDATE ON download_refresh_schedules BEGIN
 SELECT CASE WHEN (NEW.intent='explicit')!=(NEW.requested_enabled IS NOT NULL) OR (NEW.intent='suppressed' AND NEW.enabled!=0) THEN RAISE(ABORT,'invalid refresh intent') END;
 SELECT CASE WHEN OLD.intent='suppressed' AND NEW.intent!='suppressed' AND (SELECT count(*) FROM download_refresh_schedules WHERE intent!='suppressed')>=64 THEN RAISE(ABORT,'refresh schedule capacity reached') END;
END;
-- One relational projection for reconciliation; no external resource or receipt ownership.
CREATE VIEW cdh_scope_authority AS
 SELECT p.id provider_id,s.media_type,p.revision provider_revision,
        CASE WHEN p.enabled=1 AND d.enabled=1 AND coalesce(a.enabled_override,1)=1 THEN 1 ELSE 0 END desired_enabled
 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id
 JOIN completed_download_handling_settings d ON d.media_type=s.media_type
 LEFT JOIN download_processing_policies a ON a.provider_id=p.id AND a.media_type=s.media_type
 WHERE p.implementation='qbittorrent';
