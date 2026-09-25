-- nels-oss#3: validate the NOT VALID checks added to llm_usage by
-- 20260925000000_user_ai_providers.sql. sqlx 0.7 runs each migration in its own
-- transaction, so this one holds only VALIDATE's SHARE UPDATE EXCLUSIVE lock,
-- which does not block usage inserts during the scan. The ACCESS EXCLUSIVE lock
-- from ADD CONSTRAINT was released when the previous migration committed.
-- VALIDATE is idempotent: re-running it on an already-valid constraint is a no-op.
ALTER TABLE llm_usage VALIDATE CONSTRAINT llm_usage_key_source_check;
ALTER TABLE llm_usage VALIDATE CONSTRAINT llm_usage_provider_check;
