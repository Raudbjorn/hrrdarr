-- Existing release_delay_policies remains the sole global minutes/availability authority.
CREATE TABLE delay_profile_domains (
 media_type TEXT PRIMARY KEY NOT NULL CHECK(media_type IN ('tv','movies')),
 revision INTEGER NOT NULL DEFAULT 1 CHECK(typeof(revision)='integer' AND revision BETWEEN 1 AND 9007199254740991),
 locally_edited INTEGER NOT NULL DEFAULT 0 CHECK(locally_edited IN (0,1))
);
INSERT INTO delay_profile_domains(media_type)VALUES('tv'),('movies');
CREATE TABLE delay_profiles (
 id INTEGER PRIMARY KEY CHECK(id BETWEEN 1 AND 9007199254740991),
 media_type TEXT NOT NULL REFERENCES delay_profile_domains(media_type),
 is_global INTEGER NOT NULL CHECK(is_global IN (0,1)),
 position INTEGER NOT NULL CHECK(typeof(position)='integer' AND position BETWEEN 0 AND 1024),
 semantics TEXT NOT NULL CHECK(semantics IN ('legacy_age_only','profile')),
 torrent_delay_minutes INTEGER CHECK(torrent_delay_minutes IS NULL OR (typeof(torrent_delay_minutes)='integer' AND torrent_delay_minutes BETWEEN 0 AND 10080)),
 usenet_delay_minutes INTEGER CHECK(usenet_delay_minutes IS NULL OR (typeof(usenet_delay_minutes)='integer' AND usenet_delay_minutes BETWEEN 0 AND 10080)),
 enable_torrent INTEGER CHECK(enable_torrent IS NULL OR enable_torrent IN (0,1)),
 enable_usenet INTEGER CHECK(enable_usenet IS NULL OR enable_usenet IN (0,1)),
 preferred_protocol TEXT CHECK(preferred_protocol IS NULL OR preferred_protocol IN ('torrent','usenet')),
 bypass_if_highest_quality INTEGER CHECK(bypass_if_highest_quality IS NULL OR bypass_if_highest_quality IN (0,1)),
 bypass_if_above_custom_format_score INTEGER CHECK(bypass_if_above_custom_format_score IS NULL OR bypass_if_above_custom_format_score IN (0,1)),
 minimum_custom_format_score INTEGER CHECK(minimum_custom_format_score IS NULL OR (typeof(minimum_custom_format_score)='integer' AND minimum_custom_format_score BETWEEN -2147483648 AND 2147483647)),
 UNIQUE(id,media_type), UNIQUE(media_type,position),
 CHECK((is_global=1 AND position=0 AND torrent_delay_minutes IS NULL AND usenet_delay_minutes IS NULL) OR (is_global=0 AND position>0 AND torrent_delay_minutes IS NOT NULL AND usenet_delay_minutes IS NOT NULL AND semantics='profile')),
 CHECK((semantics='legacy_age_only' AND is_global=1 AND enable_torrent IS NULL AND enable_usenet IS NULL AND preferred_protocol IS NULL AND bypass_if_highest_quality IS NULL AND bypass_if_above_custom_format_score IS NULL AND minimum_custom_format_score IS NULL) OR (semantics='profile' AND enable_torrent IS NOT NULL AND enable_usenet IS NOT NULL AND (enable_torrent=1 OR enable_usenet=1) AND preferred_protocol IS NOT NULL AND bypass_if_highest_quality IS NOT NULL AND bypass_if_above_custom_format_score IS NOT NULL AND minimum_custom_format_score IS NOT NULL))
);
INSERT INTO delay_profiles(id,media_type,is_global,position,semantics)VALUES(1,'tv',1,0,'legacy_age_only'),(2,'movies',1,0,'legacy_age_only');
CREATE TRIGGER delay_global_delete BEFORE DELETE ON delay_profiles WHEN OLD.is_global=1 BEGIN SELECT RAISE(ABORT,'global delay profile cannot be deleted'); END;
CREATE TRIGGER delay_profile_identity BEFORE UPDATE ON delay_profiles WHEN NEW.id IS NOT OLD.id OR NEW.media_type IS NOT OLD.media_type OR NEW.is_global IS NOT OLD.is_global BEGIN SELECT RAISE(ABORT,'delay profile identity is immutable'); END;
CREATE TABLE delay_profile_tags (
 profile_id INTEGER NOT NULL,
 media_type TEXT NOT NULL,
 tag_id INTEGER NOT NULL UNIQUE,
 PRIMARY KEY(profile_id,tag_id),
 FOREIGN KEY(profile_id,media_type)REFERENCES delay_profiles(id,media_type)ON DELETE CASCADE,
 FOREIGN KEY(tag_id,media_type)REFERENCES tags(id,media_type)
);
CREATE TRIGGER delay_profile_tag_global BEFORE INSERT ON delay_profile_tags WHEN EXISTS(SELECT 1 FROM delay_profiles WHERE id=NEW.profile_id AND is_global=1) BEGIN SELECT RAISE(ABORT,'global delay profile cannot have tags'); END;
ALTER TABLE snapshot_imports ADD COLUMN delay_profile_version INTEGER NOT NULL DEFAULT 0 CHECK(delay_profile_version IN (0,1));
CREATE TRIGGER delay_profile_tag_identity BEFORE UPDATE ON delay_profile_tags BEGIN SELECT RAISE(ABORT,'replace delay tag membership explicitly'); END;
CREATE TRIGGER delay_profile_capacity BEFORE INSERT ON delay_profiles WHEN NOT EXISTS(SELECT 1 FROM delay_profiles WHERE id=NEW.id) AND (SELECT count(*) FROM delay_profiles WHERE media_type=NEW.media_type)>=1024 BEGIN SELECT RAISE(ABORT,'delay profile capacity reached'); END;
CREATE TRIGGER delay_profile_tag_capacity BEFORE INSERT ON delay_profile_tags WHEN (SELECT count(*) FROM delay_profile_tags WHERE profile_id=NEW.profile_id)>=200 BEGIN SELECT RAISE(ABORT,'delay tag capacity reached'); END;
