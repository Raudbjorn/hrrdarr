-- Domain-scoped naming singletons; rename stays disabled and no templates are invented until an operator configures both.
CREATE TABLE naming_settings (
    domain TEXT PRIMARY KEY NOT NULL CHECK(domain IN ('tv','movies')),
    rename_enabled INTEGER NOT NULL DEFAULT 0 CHECK(rename_enabled IN (0,1)),
    replace_illegal_characters INTEGER NOT NULL CHECK(replace_illegal_characters IN (0,1)),
    colon_replacement TEXT NOT NULL CHECK(colon_replacement IN ('delete','dash','space_dash','space_dash_space','smart','custom')),
    custom_colon_replacement TEXT,
    standard_episode_format TEXT,
    daily_episode_format TEXT,
    anime_episode_format TEXT,
    series_folder_format TEXT,
    season_folder_format TEXT,
    specials_folder_format TEXT,
    multi_episode_style INTEGER CHECK(multi_episode_style IS NULL OR multi_episode_style BETWEEN 0 AND 5),
    standard_movie_format TEXT,
    movie_folder_format TEXT,
    revision INTEGER NOT NULL CHECK (typeof(revision)='integer' AND revision BETWEEN 1 AND 9007199254740991),
    CHECK((colon_replacement='custom' AND custom_colon_replacement IS NOT NULL) OR (colon_replacement!='custom' AND custom_colon_replacement IS NULL)),
    CHECK((domain='tv' AND standard_movie_format IS NULL AND movie_folder_format IS NULL)
       OR (domain='movies' AND standard_episode_format IS NULL AND daily_episode_format IS NULL AND anime_episode_format IS NULL AND series_folder_format IS NULL AND season_folder_format IS NULL AND specials_folder_format IS NULL AND multi_episode_style IS NULL))
);
CREATE TRIGGER naming_settings_revision_step BEFORE UPDATE ON naming_settings
WHEN NEW.revision IS NOT OLD.revision+1
BEGIN SELECT RAISE(ABORT,'naming settings update requires the next revision'); END;
INSERT INTO naming_settings(domain,rename_enabled,replace_illegal_characters,colon_replacement,revision) VALUES
    ('tv',0,1,'smart',1),
    ('movies',0,1,'smart',1);
