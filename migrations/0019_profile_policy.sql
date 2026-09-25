-- Absence means unconfigured legacy policy; no inferred upgrade or cutoff defaults.
CREATE TABLE quality_profile_policies (
 profile_id INTEGER PRIMARY KEY,
 media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
 upgrade_allowed INTEGER NOT NULL CHECK(typeof(upgrade_allowed)='integer' AND upgrade_allowed IN (0,1)),
 cutoff_quality_id INTEGER,
 cutoff_group_id INTEGER,
 min_format_score INTEGER NOT NULL CHECK(typeof(min_format_score)='integer' AND min_format_score BETWEEN -2147483648 AND 0),
 cutoff_format_score INTEGER NOT NULL CHECK(typeof(cutoff_format_score)='integer' AND cutoff_format_score BETWEEN -2147483648 AND 2147483647),
 min_upgrade_format_score INTEGER NOT NULL CHECK(typeof(min_upgrade_format_score)='integer' AND min_upgrade_format_score BETWEEN 1 AND 2147483647),
 language_id INTEGER,
 FOREIGN KEY(profile_id,media_type) REFERENCES quality_profiles(id,media_type) ON DELETE CASCADE,
 FOREIGN KEY(profile_id,cutoff_quality_id) REFERENCES quality_profile_items(profile_id,quality_id),
 FOREIGN KEY(cutoff_group_id,profile_id) REFERENCES quality_profile_groups(id,profile_id),
 CHECK((cutoff_quality_id IS NOT NULL) != (cutoff_group_id IS NOT NULL)),
 CHECK((media_type='tv' AND language_id IS NULL) OR (media_type='movies' AND typeof(language_id)='integer' AND language_id BETWEEN -2 AND 57))
);
CREATE TRIGGER profile_policy_cutoff_insert BEFORE INSERT ON quality_profile_policies
WHEN NOT EXISTS(SELECT 1 FROM quality_profile_items WHERE profile_id=NEW.profile_id AND quality_id=NEW.cutoff_quality_id AND group_id IS NULL AND allowed=1)
 AND NOT EXISTS(SELECT 1 FROM quality_profile_groups WHERE profile_id=NEW.profile_id AND id=NEW.cutoff_group_id AND allowed=1)
BEGIN SELECT RAISE(ABORT,'policy cutoff must be an allowed root'); END;
CREATE TRIGGER profile_policy_cutoff_update BEFORE UPDATE ON quality_profile_policies
WHEN NOT EXISTS(SELECT 1 FROM quality_profile_items WHERE profile_id=NEW.profile_id AND quality_id=NEW.cutoff_quality_id AND group_id IS NULL AND allowed=1)
 AND NOT EXISTS(SELECT 1 FROM quality_profile_groups WHERE profile_id=NEW.profile_id AND id=NEW.cutoff_group_id AND allowed=1)
BEGIN SELECT RAISE(ABORT,'policy cutoff must be an allowed root'); END;
CREATE TRIGGER profile_policy_leaf_guard BEFORE UPDATE ON quality_profile_items
WHEN EXISTS(SELECT 1 FROM quality_profile_policies WHERE profile_id=OLD.profile_id AND cutoff_quality_id=OLD.quality_id)
 AND (NEW.allowed!=1 OR NEW.group_id IS NOT NULL)
BEGIN SELECT RAISE(ABORT,'policy cutoff must remain an allowed root'); END;
CREATE TRIGGER profile_policy_group_guard BEFORE UPDATE ON quality_profile_groups
WHEN EXISTS(SELECT 1 FROM quality_profile_policies WHERE profile_id=OLD.profile_id AND cutoff_group_id=OLD.id) AND NEW.allowed!=1
BEGIN SELECT RAISE(ABORT,'policy cutoff must remain allowed'); END;
