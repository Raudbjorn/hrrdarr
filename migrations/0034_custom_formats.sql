-- Definitions are read atomically; queried profile relationships remain relational.
-- Specification JSON version 1 uses db::custom_formats::Specification.
CREATE TABLE custom_formats (
 id INTEGER PRIMARY KEY,
 media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
 name TEXT NOT NULL CHECK(typeof(name)='text' AND length(trim(name)) BETWEEN 1 AND 100 AND instr(name,char(0))=0),
 include_when_renaming INTEGER NOT NULL CHECK(typeof(include_when_renaming)='integer' AND include_when_renaming IN (0,1)),
 specification_version INTEGER NOT NULL DEFAULT 1 CHECK(specification_version=1),
 specifications_json TEXT NOT NULL CHECK(typeof(specifications_json)='text' AND length(CAST(specifications_json AS BLOB))<=65536 AND json_valid(specifications_json) AND json_type(specifications_json)='array' AND json_array_length(specifications_json) BETWEEN 1 AND 64),
 UNIQUE(id,media_type),
 UNIQUE(media_type,name)
);
CREATE TABLE quality_profile_format_scores (
 profile_id INTEGER NOT NULL,
 format_id INTEGER NOT NULL,
 media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
 score INTEGER NOT NULL CHECK(typeof(score)='integer' AND score BETWEEN -2147483648 AND 2147483647),
 PRIMARY KEY(profile_id,format_id),
 FOREIGN KEY(profile_id,media_type) REFERENCES quality_profiles(id,media_type) ON DELETE CASCADE,
 FOREIGN KEY(format_id,media_type) REFERENCES custom_formats(id,media_type) ON DELETE CASCADE
);
CREATE INDEX custom_format_profile_scores ON quality_profile_format_scores(format_id,media_type);
-- Positive thresholds become meaningful with nonempty format scores. Preserve all existing policy.
CREATE TABLE new_quality_profile_policies (
 profile_id INTEGER PRIMARY KEY,
 media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
 upgrade_allowed INTEGER NOT NULL CHECK(typeof(upgrade_allowed)='integer' AND upgrade_allowed IN (0,1)),
 cutoff_quality_id INTEGER,
 cutoff_group_id INTEGER,
 min_format_score INTEGER NOT NULL CHECK(typeof(min_format_score)='integer' AND min_format_score BETWEEN -2147483648 AND 2147483647),
 cutoff_format_score INTEGER NOT NULL CHECK(typeof(cutoff_format_score)='integer' AND cutoff_format_score BETWEEN -2147483648 AND 2147483647),
 min_upgrade_format_score INTEGER NOT NULL CHECK(typeof(min_upgrade_format_score)='integer' AND min_upgrade_format_score BETWEEN 1 AND 2147483647),
 language_id INTEGER,
 FOREIGN KEY(profile_id,media_type) REFERENCES quality_profiles(id,media_type) ON DELETE CASCADE,
 FOREIGN KEY(profile_id,cutoff_quality_id) REFERENCES quality_profile_items(profile_id,quality_id),
 FOREIGN KEY(cutoff_group_id,profile_id) REFERENCES quality_profile_groups(id,profile_id),
 CHECK((cutoff_quality_id IS NOT NULL) != (cutoff_group_id IS NOT NULL)),
 CHECK((media_type='tv' AND language_id IS NULL) OR (media_type='movies' AND typeof(language_id)='integer' AND language_id BETWEEN -2 AND 57))
);
INSERT INTO new_quality_profile_policies SELECT * FROM quality_profile_policies;
DROP TRIGGER profile_policy_cutoff_insert;
DROP TRIGGER profile_policy_cutoff_update;
DROP TRIGGER profile_policy_leaf_guard;
DROP TRIGGER profile_policy_group_guard;
DROP TABLE quality_profile_policies;
ALTER TABLE new_quality_profile_policies RENAME TO quality_profile_policies;
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
-- Missing facts remain unknown. A renamed path is not the original release title.
ALTER TABLE file_metadata ADD COLUMN original_release_title TEXT CHECK(original_release_title IS NULL OR (typeof(original_release_title)='text' AND length(CAST(original_release_title AS BLOB)) BETWEEN 1 AND 4096 AND instr(original_release_title,char(0))=0));
ALTER TABLE series ADD COLUMN original_language INTEGER CHECK(original_language IS NULL OR (typeof(original_language)='integer' AND original_language BETWEEN 0 AND 52));
