-- Keep collection policy references valid when their profile is changed or deleted.
CREATE TRIGGER collection_profile_delete BEFORE DELETE ON quality_profiles
WHEN EXISTS(SELECT 1 FROM movie_collections WHERE quality_profile_id=OLD.id)
BEGIN SELECT RAISE(ABORT,'collection_profile_in_use'); END;
CREATE TRIGGER collection_profile_identity_update BEFORE UPDATE OF id,media_type ON quality_profiles
WHEN (NEW.id!=OLD.id OR NEW.media_type!='movies')
 AND EXISTS(SELECT 1 FROM movie_collections WHERE quality_profile_id=OLD.id)
BEGIN SELECT RAISE(ABORT,'collection_profile_in_use'); END;

-- These clients use the category as a directory component, not just a remote label.
CREATE TRIGGER directory_client_category_insert BEFORE INSERT ON provider_scopes
WHEN NEW.implementation IN ('aria2','downloadstation') AND NEW.category IN ('.','..')
BEGIN SELECT RAISE(ABORT,'invalid directory client category'); END;
CREATE TRIGGER directory_client_category_update BEFORE UPDATE ON provider_scopes
WHEN NEW.implementation IN ('aria2','downloadstation') AND NEW.category IN ('.','..')
BEGIN SELECT RAISE(ABORT,'invalid directory client category'); END;
-- Refuse unsafe legacy values before upgrading; never invent a different remote directory.
UPDATE provider_scopes SET category=category
WHERE implementation IN ('aria2','downloadstation') AND category IN ('.','..');

-- Every claim gets a fresh token, independent of retry accounting, to fence stale senders.
ALTER TABLE notification_deliveries ADD COLUMN lease_owner TEXT;
