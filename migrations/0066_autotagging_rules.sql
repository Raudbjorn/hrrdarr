-- AutoTagging rules (api.002). Rules are scoped to one media domain; conditions are structured JSON
-- validated by the application (src/autotagging.rs), tags are relational so tag deletion is protected.
CREATE TABLE autotagging_rules (
 id INTEGER PRIMARY KEY AUTOINCREMENT CHECK(id BETWEEN 1 AND 9007199254740991),
 media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
 name TEXT NOT NULL CHECK(typeof(name)='text' AND length(CAST(name AS BLOB)) BETWEEN 1 AND 256 AND instr(name,char(0))=0 AND length(trim(name))>0),
 remove_tags_automatically INTEGER NOT NULL DEFAULT 0 CHECK(remove_tags_automatically IN (0,1)),
 UNIQUE(media_type,name),
 UNIQUE(id,media_type)
);
CREATE TABLE autotagging_rule_tags (
 rule_id INTEGER NOT NULL,
 media_type TEXT NOT NULL,
 tag_id INTEGER NOT NULL,
 PRIMARY KEY(rule_id,tag_id),
 FOREIGN KEY(rule_id,media_type) REFERENCES autotagging_rules(id,media_type) ON DELETE CASCADE,
 FOREIGN KEY(tag_id,media_type) REFERENCES tags(id,media_type) ON DELETE RESTRICT
);
CREATE INDEX autotagging_rule_tags_tag ON autotagging_rule_tags(tag_id,rule_id);
CREATE TABLE autotagging_specifications (
 id INTEGER PRIMARY KEY AUTOINCREMENT,
 rule_id INTEGER NOT NULL REFERENCES autotagging_rules(id) ON DELETE CASCADE,
 position INTEGER NOT NULL CHECK(position BETWEEN 0 AND 63),
 name TEXT NOT NULL CHECK(typeof(name)='text' AND length(CAST(name AS BLOB)) BETWEEN 1 AND 256 AND instr(name,char(0))=0 AND length(trim(name))>0),
 negate INTEGER NOT NULL CHECK(negate IN (0,1)),
 required INTEGER NOT NULL CHECK(required IN (0,1)),
 -- Tagged enum {"implementation":"genre","values":[...]}; the application owns per-implementation validation.
 condition_json TEXT NOT NULL CHECK(typeof(condition_json)='text' AND json_valid(condition_json) AND json_type(condition_json)='object' AND json_type(condition_json,'$.implementation')='text' AND length(CAST(condition_json AS BLOB))<=16384),
 UNIQUE(rule_id,position)
);
CREATE TRIGGER autotagging_rules_limit BEFORE INSERT ON autotagging_rules
WHEN (SELECT count(*) FROM autotagging_rules WHERE media_type=NEW.media_type)>=256
BEGIN SELECT RAISE(ABORT,'autotagging rule limit reached'); END;
CREATE TRIGGER autotagging_rule_tags_limit BEFORE INSERT ON autotagging_rule_tags
WHEN (SELECT count(*) FROM autotagging_rule_tags WHERE rule_id=NEW.rule_id)>=64
BEGIN SELECT RAISE(ABORT,'autotagging tag limit reached'); END;
CREATE TRIGGER autotagging_specifications_limit BEFORE INSERT ON autotagging_specifications
WHEN (SELECT count(*) FROM autotagging_specifications WHERE rule_id=NEW.rule_id)>=64
BEGIN SELECT RAISE(ABORT,'autotagging specification limit reached'); END;
