-- Native mixed-domain History reads order by the stored UTC commit time and stable event ID.
-- Retain target-specific indices; no historical facts or event semantics change.
CREATE INDEX import_history_order ON import_history(imported_at DESC,operation_id DESC);
