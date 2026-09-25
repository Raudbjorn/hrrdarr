-- Optional policy assignments for existing libraries; no startup defaults are invented.
CREATE TABLE library_settings (
    id INTEGER PRIMARY KEY,
    media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
    series_id INTEGER UNIQUE REFERENCES series(id) ON DELETE RESTRICT,
    movie_id INTEGER UNIQUE REFERENCES movies(id) ON DELETE RESTRICT,
    quality_profile_id INTEGER,
    series_type TEXT CHECK(series_type IN ('standard','daily','anime')),
    season_folder INTEGER CHECK(season_folder IN (0,1)),
    use_scene_numbering INTEGER CHECK(use_scene_numbering IN (0,1)),
    monitor_new_items TEXT CHECK(monitor_new_items IN ('all','none')),
    minimum_availability TEXT CHECK(minimum_availability IN ('tba','announced','in_cinemas','released')),
    added TEXT,
    CHECK((media_type='tv' AND series_id IS NOT NULL AND movie_id IS NULL AND minimum_availability IS NULL)
       OR (media_type='movies' AND movie_id IS NOT NULL AND series_id IS NULL AND series_type IS NULL AND season_folder IS NULL AND use_scene_numbering IS NULL AND monitor_new_items IS NULL)),
    FOREIGN KEY(quality_profile_id,media_type) REFERENCES quality_profiles(id,media_type) ON DELETE RESTRICT
);
CREATE INDEX series_path_lookup ON series(path);
