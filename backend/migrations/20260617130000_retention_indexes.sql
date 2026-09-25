-- Retention purge support (issue #45): the hourly time-based purges in
-- `purge_old_audit_logs` / `purge_old_notifications` filter by `created_at`
-- (and `is_read` for notifications). Without these indexes the DELETEs
-- seq-scan the whole table every hour, a load spike that grows with the table.
-- Indexes are additive and backward compatible (no data change).

-- audit_logs has no index supporting the age predicate.
CREATE INDEX IF NOT EXISTS audit_logs_created_at_idx
    ON audit_logs (created_at);

-- notifications only indexes (user_id, created_at DESC), which does not serve a
-- table-wide (is_read, created_at) purge predicate. Index both columns so the
-- planner can satisfy each arm of the read/unread OR.
CREATE INDEX IF NOT EXISTS notifications_is_read_created_at_idx
    ON notifications (is_read, created_at);
