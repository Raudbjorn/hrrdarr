-- Activation is validated on exact upload, never inferred from an old raw archive.
ALTER TABLE snapshot_imports ADD COLUMN profile_version INTEGER NOT NULL DEFAULT 0 CHECK(profile_version IN (0,1));
