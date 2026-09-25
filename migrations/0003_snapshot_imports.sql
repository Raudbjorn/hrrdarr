-- Raw source records are private archival state, never activated provider/client configuration.
CREATE TABLE snapshot_imports (
    application TEXT NOT NULL CHECK (application IN ('sonarr', 'radarr')),
    fingerprint TEXT NOT NULL,
    schema_version INTEGER NOT NULL,
    imported_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (application, fingerprint)
);
CREATE TABLE snapshot_records (
    application TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    source_table TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    record_json TEXT NOT NULL CHECK (json_valid(record_json)),
    PRIMARY KEY (application, fingerprint, source_table, ordinal),
    FOREIGN KEY (application, fingerprint) REFERENCES snapshot_imports(application, fingerprint)
);
CREATE TABLE snapshot_mappings (
    application TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    destination_table TEXT NOT NULL,
    source_id INTEGER NOT NULL,
    destination_id INTEGER NOT NULL,
    PRIMARY KEY (application, fingerprint, destination_table, source_id),
    FOREIGN KEY (application, fingerprint) REFERENCES snapshot_imports(application, fingerprint)
);
