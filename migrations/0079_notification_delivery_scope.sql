-- Retain the scope whose filters selected a notification, including system events and retries.
-- NULL identifies legacy deliveries whose original selection was not recorded.
ALTER TABLE notification_deliveries ADD COLUMN media_type TEXT
 CHECK(media_type IS NULL OR media_type IN ('tv','movies'));
