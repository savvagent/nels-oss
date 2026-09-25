-- Issue #44 (follow-up from #42): the hourly purge_stale_embeddings (backend/src/rag.rs)
-- ranks rows with row_number() OVER (PARTITION BY conversation_id ORDER BY created_at DESC)
-- over the subset WHERE embedding IS NOT NULL. The existing
-- idx_chat_messages_conversation (conversation_id, created_at) already serves the window
-- ordering and lets the planner apply "embedding IS NOT NULL" as a recheck, so this is NOT
-- a "full table rank". The win this partial index adds is at scale: once a meaningful
-- fraction of rows have been purged (embedding set to NULL), the full index visits those
-- dead rows while this partial index spans only the live-embedding subset the job actually
-- scans, in the window's sort order. At small NULL fractions the two are near-equivalent and
-- the planner may pick either; this index pays off as the cleared-row fraction grows, which
-- is the at-large-volume case #44 targets.
--
-- Tradeoff: this duplicates the column structure of idx_chat_messages_conversation, so
-- writes that set/clear embedding maintain both btrees (the purge's UPDATE ... SET
-- embedding = NULL deletes each row from this partial index). Accepted: the per-hour scan
-- cost dominates at the volumes that motivate the change.
--
-- NOT built CONCURRENTLY: sqlx runs each migration in a transaction and CREATE INDEX
-- CONCURRENTLY cannot run in one. This matches the repo's existing index migrations
-- (#45 retention_indexes, idx_chat_messages_conversation). The brief SHARE lock at
-- startup (blocks writes, allows reads) is acceptable at current scale; a future
-- CONCURRENT rebuild via a
-- `-- no-transaction` migration is the escalation path if the table grows large.
-- Additive and backward compatible (no data change).
CREATE INDEX IF NOT EXISTS idx_chat_messages_embedding_purge
    ON chat_messages (conversation_id, created_at)
    WHERE embedding IS NOT NULL;
