-- Global conversation threads. A thread spans budgets; the active budget is
-- context within a thread (switched via chat), not a property of the thread.
CREATE TABLE IF NOT EXISTS conversations (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    title TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Recency sort for the sidebar list (newest-updated first per user).
CREATE INDEX IF NOT EXISTS idx_conversations_user_updated
    ON conversations(user_id, updated_at DESC);

-- Foreign-key chat messages to their thread. Nullable for safety; messages
-- created before this migration are backfilled below.
ALTER TABLE chat_messages
    ADD COLUMN IF NOT EXISTS conversation_id UUID REFERENCES conversations(id) ON DELETE CASCADE;

CREATE INDEX IF NOT EXISTS idx_chat_messages_conversation
    ON chat_messages(conversation_id, created_at);

-- Backfill: collapse each user's pre-existing messages into one
-- "Earlier conversation" thread. Idempotent (only touches NULL conversation_id
-- rows) and safe on an empty database. gen_random_uuid() is core in PG16.
WITH new_convos AS (
    INSERT INTO conversations (id, user_id, title, created_at, updated_at)
    SELECT gen_random_uuid(), user_id, 'Earlier conversation', MIN(created_at), MAX(created_at)
    FROM chat_messages
    WHERE conversation_id IS NULL
    GROUP BY user_id
    RETURNING id, user_id
)
UPDATE chat_messages cm
SET conversation_id = nc.id
FROM new_convos nc
WHERE cm.user_id = nc.user_id AND cm.conversation_id IS NULL;
