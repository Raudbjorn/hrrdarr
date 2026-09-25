ALTER TABLE operations RENAME TO legacy_operations;
ALTER TABLE episodes RENAME TO legacy_episodes;
ALTER TABLE series RENAME TO legacy_series;

CREATE TABLE series (
    id INTEGER PRIMARY KEY,
    tvdb_id INTEGER UNIQUE CHECK (tvdb_id > 0),
    title TEXT NOT NULL,
    year INTEGER,
    path TEXT NOT NULL CHECK (length(trim(path)) > 0),
    poster TEXT,
    monitored INTEGER NOT NULL DEFAULT 1 CHECK (monitored IN (0, 1))
);
CREATE TABLE seasons (
    series_id INTEGER NOT NULL REFERENCES series(id),
    number INTEGER NOT NULL CHECK (number >= 0),
    monitored INTEGER NOT NULL DEFAULT 1 CHECK (monitored IN (0, 1)),
    PRIMARY KEY (series_id, number)
);
CREATE TABLE episode_files (
    id INTEGER PRIMARY KEY,
    series_id INTEGER NOT NULL REFERENCES series(id),
    path TEXT NOT NULL UNIQUE CHECK (length(trim(path)) > 0),
    UNIQUE (id, series_id)
);
CREATE TABLE episodes (
    id INTEGER PRIMARY KEY,
    series_id INTEGER NOT NULL,
    season INTEGER NOT NULL,
    number INTEGER NOT NULL CHECK (number >= 0),
    title TEXT NOT NULL,
    episode_file_id INTEGER,
    monitored INTEGER NOT NULL DEFAULT 1 CHECK (monitored IN (0, 1)),
    UNIQUE (series_id, season, number),
    FOREIGN KEY (series_id, season) REFERENCES seasons(series_id, number),
    FOREIGN KEY (episode_file_id, series_id) REFERENCES episode_files(id, series_id)
);
CREATE INDEX episodes_file ON episodes(episode_file_id, series_id);

CREATE TABLE movie_metadata (
    id INTEGER PRIMARY KEY,
    tmdb_id INTEGER UNIQUE CHECK (tmdb_id > 0),
    imdb_id TEXT UNIQUE CHECK (length(trim(imdb_id)) > 0),
    title TEXT NOT NULL,
    year INTEGER
);
CREATE TABLE movies (
    id INTEGER PRIMARY KEY,
    metadata_id INTEGER NOT NULL UNIQUE REFERENCES movie_metadata(id),
    path TEXT NOT NULL UNIQUE CHECK (length(trim(path)) > 0),
    monitored INTEGER NOT NULL DEFAULT 1 CHECK (monitored IN (0, 1))
);
CREATE TABLE movie_files (
    id INTEGER PRIMARY KEY,
    movie_id INTEGER NOT NULL UNIQUE REFERENCES movies(id),
    path TEXT NOT NULL UNIQUE CHECK (length(trim(path)) > 0),
    edition TEXT
);
CREATE TABLE movie_collections (
    id INTEGER PRIMARY KEY,
    tmdb_id INTEGER UNIQUE CHECK (tmdb_id > 0),
    title TEXT NOT NULL
);
CREATE TABLE movie_collection_members (
    collection_id INTEGER NOT NULL REFERENCES movie_collections(id),
    metadata_id INTEGER NOT NULL REFERENCES movie_metadata(id),
    PRIMARY KEY (collection_id, metadata_id)
);
CREATE INDEX movie_collection_members_metadata ON movie_collection_members(metadata_id);

CREATE TABLE operations (
    id TEXT PRIMARY KEY NOT NULL,
    media_type TEXT NOT NULL CHECK (media_type IN ('episode', 'movie')),
    episode_id INTEGER REFERENCES episodes(id),
    movie_id INTEGER REFERENCES movies(id),
    source TEXT NOT NULL,
    mode TEXT NOT NULL,
    destination TEXT NOT NULL,
    status TEXT NOT NULL,
    message TEXT NOT NULL,
    CHECK ((media_type = 'episode' AND episode_id IS NOT NULL AND movie_id IS NULL)
        OR (media_type = 'movie' AND movie_id IS NOT NULL AND episode_id IS NULL))
);
CREATE INDEX operations_episode ON operations(episode_id);
CREATE INDEX operations_movie ON operations(movie_id);

INSERT INTO series (id, title, year, path, poster)
SELECT id, title, year, path, poster FROM legacy_series;
INSERT INTO seasons (series_id, number)
SELECT DISTINCT series_id, season FROM legacy_episodes;
INSERT INTO episode_files (series_id, path)
SELECT DISTINCT series_id, file_path FROM legacy_episodes WHERE file_path IS NOT NULL;
INSERT INTO episodes (id, series_id, season, number, title, episode_file_id)
SELECT e.id, e.series_id, e.season, e.number, e.title, f.id
FROM legacy_episodes e LEFT JOIN episode_files f ON f.series_id = e.series_id AND f.path = e.file_path;
INSERT INTO operations (id, media_type, episode_id, source, mode, destination, status, message)
SELECT id, 'episode', episode_id, source, mode, destination, status, message FROM legacy_operations;

DROP TABLE legacy_operations;
DROP TABLE legacy_episodes;
DROP TABLE legacy_series;
