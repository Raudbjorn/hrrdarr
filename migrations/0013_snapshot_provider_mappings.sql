-- Provider IDs are UUIDs; source table distinguishes equal indexer/client numeric IDs.
-- No provider FK: deletion remains allowed and replay detects the missing destination.
CREATE TABLE snapshot_provider_mappings (
    application TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    source_table TEXT NOT NULL CHECK(source_table IN ('Indexers','DownloadClients')),
    source_id INTEGER NOT NULL CHECK(typeof(source_id)='integer' AND source_id>0),
    provider_id TEXT NOT NULL CHECK(length(provider_id)=36 AND substr(provider_id,9,1)='-' AND substr(provider_id,14,1)='-' AND substr(provider_id,19,1)='-' AND substr(provider_id,24,1)='-' AND length(replace(provider_id,'-',''))=32 AND replace(provider_id,'-','') NOT GLOB '*[^0-9a-f]*'),
    provider_revision INTEGER NOT NULL CHECK(typeof(provider_revision)='integer' AND provider_revision BETWEEN 1 AND 9007199254740991),
    PRIMARY KEY(application,fingerprint,source_table,source_id),
    FOREIGN KEY(application,fingerprint) REFERENCES snapshot_imports(application,fingerprint)
);
