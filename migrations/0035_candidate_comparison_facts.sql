-- Non-secret versioned facts survive destruction of the private provider payload.
-- NULL is unknown for legacy receipts; it is never backfilled from a later filename.
ALTER TABLE rss_candidates ADD COLUMN comparison_facts_json TEXT
 CHECK(comparison_facts_json IS NULL OR (
  typeof(comparison_facts_json)='text' AND length(CAST(comparison_facts_json AS BLOB))<=16384
  AND json_valid(comparison_facts_json) AND json_type(comparison_facts_json)='object'
  AND json_type(comparison_facts_json,'$.version') IS 'integer'
  AND json_extract(comparison_facts_json,'$.version')=1
 ));
CREATE TRIGGER rss_comparison_facts_insert BEFORE INSERT ON rss_candidates
WHEN NEW.comparison_facts_json IS NOT NULL
BEGIN SELECT RAISE(ABORT,'comparison facts require submission transition'); END;
CREATE TRIGGER rss_comparison_facts_update BEFORE UPDATE ON rss_candidates
WHEN NEW.comparison_facts_json IS NOT OLD.comparison_facts_json
 AND NOT (OLD.status='prepared' AND NEW.status='submitting'
          AND OLD.comparison_facts_json IS NULL AND NEW.comparison_facts_json IS NOT NULL)
BEGIN SELECT RAISE(ABORT,'submitted comparison facts are immutable'); END;
