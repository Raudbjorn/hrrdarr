-- Durable Wanted search admission receipts. Every Wanted search request, including one that
-- matches no target or only targets that were already being searched, records its request
-- UUID, canonical-body digest, domain, result counters and admitted target identities in the
-- same Immediate transaction that inserts the child search commands. A retry authenticates its
-- UUID and body against this record before it captures targets, so it neither re-evaluates the
-- list nor depends on child command history (which housekeeping prunes).
--
-- Retention: a receipt is authoritative for 7 days from creation; after expires_at the UUID is
-- explicit-expiry territory and may be admitted again. Capacity: at most 4096 unexpired
-- receipts and 65536 unexpired admitted targets, enforced below and pre-checked by the writer.
-- Expired receipts are swept at admission and by terminal-history housekeeping; an unexpired
-- receipt is never deleted to make room. The literals are kept in step with
-- src/commands/search.rs (WANTED_RECEIPT_*).
-- Provider credentials and payloads are deliberately absent: the digest binds the provider
-- identity, revision and client selection without storing them.
CREATE TABLE wanted_search_receipts (
 request_id TEXT PRIMARY KEY NOT NULL CHECK(typeof(request_id)='text' AND length(request_id)=36 AND substr(request_id,9,1)='-' AND substr(request_id,14,1)='-' AND substr(request_id,19,1)='-' AND substr(request_id,24,1)='-' AND length(replace(request_id,'-',''))=32 AND replace(request_id,'-','') NOT GLOB '*[^0-9a-f]*'),
 media_type TEXT NOT NULL CHECK(media_type IN ('tv','movies')),
 digest BLOB NOT NULL CHECK(typeof(digest)='blob' AND length(digest)=32),
 matched INTEGER NOT NULL CHECK(typeof(matched)='integer' AND matched BETWEEN 0 AND 1024),
 skipped_active INTEGER NOT NULL CHECK(typeof(skipped_active)='integer' AND skipped_active BETWEEN 0 AND 1024),
 admitted INTEGER NOT NULL CHECK(typeof(admitted)='integer' AND admitted BETWEEN 0 AND 1024),
 created_at INTEGER NOT NULL CHECK(typeof(created_at)='integer' AND created_at BETWEEN 0 AND 9007199254740991),
 expires_at INTEGER NOT NULL CHECK(typeof(expires_at)='integer' AND expires_at BETWEEN 0 AND 9007199254740991),
 CHECK(matched=admitted+skipped_active),
 CHECK(expires_at>created_at)
);
CREATE INDEX wanted_search_receipts_expiry ON wanted_search_receipts(media_type,expires_at,request_id);
-- Target identities are not foreign keys: a deleted series, movie or pruned child command must
-- not weaken a retained receipt. The command id is the deterministic child id for the receipt.
CREATE TABLE wanted_search_receipt_targets (
 request_id TEXT NOT NULL REFERENCES wanted_search_receipts(request_id) ON DELETE CASCADE,
 target_id INTEGER NOT NULL CHECK(typeof(target_id)='integer' AND target_id BETWEEN 1 AND 9007199254740991),
 command_id TEXT NOT NULL CHECK(typeof(command_id)='text' AND length(command_id)=36),
 PRIMARY KEY(request_id,target_id)
) WITHOUT ROWID;
CREATE TRIGGER wanted_search_receipts_admit BEFORE INSERT ON wanted_search_receipts
BEGIN
 SELECT CASE WHEN (SELECT count(*) FROM wanted_search_receipts WHERE expires_at>NEW.created_at)>=4096 THEN RAISE(ABORT,'wanted receipt capacity reached') END;
 SELECT CASE WHEN (SELECT coalesce(sum(admitted),0) FROM wanted_search_receipts WHERE expires_at>NEW.created_at)+NEW.admitted>65536 THEN RAISE(ABORT,'wanted receipt target capacity reached') END;
END;
-- A receipt is the authority for one admission; it is written once and only ever deleted.
CREATE TRIGGER wanted_search_receipts_immutable BEFORE UPDATE ON wanted_search_receipts
BEGIN SELECT RAISE(ABORT,'wanted receipts are immutable'); END;
CREATE TRIGGER wanted_search_receipt_targets_immutable BEFORE UPDATE ON wanted_search_receipt_targets
BEGIN SELECT RAISE(ABORT,'wanted receipts are immutable'); END;
