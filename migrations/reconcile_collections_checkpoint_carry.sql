-- The checkpoint's policy now lives on movie_collections (0066) with the ownership markers the
-- snapshot migration added (0069). A checkpoint row was only monitored with a complete policy, so
-- the carried values satisfy this branch's own policy guard.
UPDATE movie_collections SET
    monitored
    = coalesce((
        SELECT p.monitored FROM collections_checkpoint_policy AS p
        WHERE p.collection_id = movie_collections.id
    ), monitored),
    root_folder_id
    = coalesce((
        SELECT p.root_folder_id FROM collections_checkpoint_policy AS p
        WHERE p.collection_id = movie_collections.id
    ), root_folder_id),
    quality_profile_id
    = coalesce((
        SELECT p.quality_profile_id FROM collections_checkpoint_policy AS p
        WHERE p.collection_id = movie_collections.id
    ), quality_profile_id),
    minimum_availability
    = coalesce((
        SELECT p.minimum_availability FROM collections_checkpoint_policy AS p
        WHERE p.collection_id = movie_collections.id
    ), minimum_availability),
    search_on_add
    = coalesce((
        SELECT p.search_on_add FROM collections_checkpoint_policy AS p
        WHERE p.collection_id = movie_collections.id
    ), search_on_add),
    local_edit
    = max(local_edit, coalesce((
        SELECT p.local_edit FROM collections_checkpoint_policy AS p
        WHERE p.collection_id = movie_collections.id
    ), 0)),
    snapshot_owned
    = coalesce((
        SELECT p.snapshot_owned FROM collections_checkpoint_policy AS p
        WHERE p.collection_id = movie_collections.id
    ), snapshot_owned)
WHERE id IN (SELECT collection_id FROM collections_checkpoint_policy);
DROP TABLE collections_checkpoint_policy;
