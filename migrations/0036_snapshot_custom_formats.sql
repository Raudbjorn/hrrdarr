-- Exact upload activates supported custom-format graphs; opening old archives does not.
ALTER TABLE snapshot_imports ADD COLUMN custom_format_version INTEGER NOT NULL DEFAULT 0 CHECK(custom_format_version IN (0,1));
