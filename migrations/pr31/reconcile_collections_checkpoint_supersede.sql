-- Supersedes origin/main's collections checkpoint (its published 0052). That checkpoint kept the
-- per-collection policy in its own settings table and guarded it with its own triggers; this
-- branch keeps the policy inline on movie_collections (0066) and owns its own tag table. The
-- checkpoint's policy and tags are carried across first, so nothing a person configured is lost.
CREATE TABLE collections_checkpoint_policy (
    collection_id INTEGER PRIMARY KEY REFERENCES movie_collections (
        id
    ) ON DELETE RESTRICT,
    monitored INTEGER NOT NULL CHECK (monitored IN (0, 1)),
    root_folder_id INTEGER REFERENCES root_folders (id) ON DELETE RESTRICT,
    quality_profile_id INTEGER,
    minimum_availability TEXT CHECK (
        minimum_availability IN ('tba', 'announced', 'in_cinemas', 'released')
    ),
    search_on_add INTEGER NOT NULL CHECK (search_on_add IN (0, 1)),
    local_edit INTEGER NOT NULL CHECK (local_edit IN (0, 1)),
    snapshot_owned INTEGER NOT NULL CHECK (snapshot_owned IN (0, 1)),
    settings_revision INTEGER NOT NULL
);
INSERT INTO collections_checkpoint_policy (
    collection_id,
    monitored,
    root_folder_id,
    quality_profile_id,
    minimum_availability,
    search_on_add,
    local_edit,
    snapshot_owned,
    settings_revision
)
SELECT
    collection_id,
    monitored,
    root_folder_id,
    quality_profile_id,
    minimum_availability,
    coalesce(search_on_add, 0),
    local_edit,
    snapshot_owned,
    settings_revision
FROM movie_collection_settings;
CREATE TABLE collections_checkpoint_tags (
    collection_id INTEGER NOT NULL,
    tag_id INTEGER NOT NULL
);
INSERT INTO collections_checkpoint_tags (collection_id, tag_id)
SELECT
    collection_id,
    tag_id
FROM movie_collection_tags;
-- The checkpoint's guards belong to its own model: this branch's model replaces them.
DROP TRIGGER collections_identity_insert;
DROP TRIGGER collections_identity_update;
DROP TRIGGER collections_generated_id;
DROP TRIGGER collections_metadata_revision;
DROP TRIGGER collections_images_insert;
DROP TRIGGER collections_images_update;
DROP TRIGGER collection_root_domain_update;
DROP TRIGGER collection_members_insert;
DROP TRIGGER collection_members_update;
DROP TRIGGER movie_import_exclusions_revision;
DROP TRIGGER movie_import_exclusions_retain;
-- The tag table goes before the settings table it referenced; both are replaced by this
-- branch's model, whose tag table points at the collection itself.
DROP TABLE movie_collection_tags;
DROP TABLE movie_collection_settings;
-- Removal intent is independent of catalog membership and remains authoritative.
CREATE TABLE movie_collection_tags (
    collection_id INTEGER NOT NULL REFERENCES movie_collections (
        id
    ) ON DELETE CASCADE,
    tag_id INTEGER NOT NULL,
    media_type TEXT NOT NULL DEFAULT 'movies' CHECK (media_type = 'movies'),
    PRIMARY KEY (collection_id, tag_id),
    FOREIGN KEY (tag_id, media_type) REFERENCES tags (
        id, media_type
    ) ON DELETE RESTRICT
);
CREATE INDEX movie_collection_tags_tag ON movie_collection_tags (
    tag_id, collection_id
);
INSERT INTO movie_collection_tags (collection_id, tag_id)
SELECT
    collection_id,
    tag_id
FROM collections_checkpoint_tags;
DROP TABLE collections_checkpoint_tags;
