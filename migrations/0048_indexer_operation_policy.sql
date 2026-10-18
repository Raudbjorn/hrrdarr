-- Native indexer defaults preserve existing behavior without mutating scopes or observations.
-- Client values are inert: eligibility still requires indexer implementation, master enable and scope.
ALTER TABLE provider_scopes ADD COLUMN enable_rss INTEGER NOT NULL DEFAULT 1 CHECK(typeof(enable_rss)='integer' AND enable_rss IN (0,1));
ALTER TABLE provider_scopes ADD COLUMN enable_automatic_search INTEGER NOT NULL DEFAULT 1 CHECK(typeof(enable_automatic_search)='integer' AND enable_automatic_search IN (0,1));
ALTER TABLE provider_scopes ADD COLUMN enable_interactive_search INTEGER NOT NULL DEFAULT 1 CHECK(typeof(enable_interactive_search)='integer' AND enable_interactive_search IN (0,1));
INSERT INTO health_checks(scope,check_key,startup,scheduled,compatibility_type)VALUES
 ('tv','indexer_search',1,1,'IndexerSearchCheck'),
 ('movies','indexer_search',1,1,'IndexerSearchCheck'),
 ('tv','indexer_rss',1,1,'IndexerRssCheck'),
 ('movies','indexer_rss',1,1,'IndexerRssCheck');
