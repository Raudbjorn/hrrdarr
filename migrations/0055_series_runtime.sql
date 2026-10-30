-- Unknown series runtime remains NULL; zero is an observed catalog value.
-- TV minutes retain the native episode / source signed-int range.
ALTER TABLE series ADD COLUMN runtime INTEGER
 CHECK(runtime IS NULL OR (typeof(runtime)='integer' AND runtime BETWEEN 0 AND 2147483647));
-- Historical exact snapshot replays may acquire this newly supported field once.
ALTER TABLE snapshot_imports ADD COLUMN series_runtime_version INTEGER NOT NULL DEFAULT 0
 CHECK(series_runtime_version IN (0,1));
