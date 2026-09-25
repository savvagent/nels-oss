-- Supports the per-user MIN/MAX(created_at) correlated subqueries in
-- GET /api/admin/users (backend/src/admin.rs), which filter chat_messages by
-- user_id and aggregate created_at. Existing chat_messages indexes are keyed on
-- conversation_id, so without this a per-user activity scan degrades on large
-- message volumes. DESC on created_at lets the MAX(...) probe terminate early.
CREATE INDEX IF NOT EXISTS chat_messages_user_id_created_idx
    ON chat_messages (user_id, created_at DESC);
